//! Provider keys, connector and sign-in tokens, link and archive keys: kept in the OS keychain
//! (headless CI only: a plaintext file), never in SQLite, a sandbox, a log or an event.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use crate::error::ApiError;

const SERVICE: &str = "ferrobot";

#[derive(Clone)]
pub enum SecretStore {
    /// macOS Keychain, Windows Credential Manager or the Secret Service.
    Keyring,
    /// Plaintext JSON file (mode 0600). For headless CI without a keychain.
    File(PathBuf, Arc<Mutex<()>>),
    /// In memory; for tests and embedding.
    Memory(Arc<Mutex<BTreeMap<String, String>>>),
}

impl std::fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::Keyring => "keyring",
            Self::File(..) => "file",
            Self::Memory(_) => "memory",
        };
        write!(f, "SecretStore({kind})")
    }
}

impl Default for SecretStore {
    fn default() -> Self {
        Self::Memory(Arc::default())
    }
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(format!("secret store: {e}"))
}

impl SecretStore {
    pub fn file(path: PathBuf) -> Self {
        Self::File(path, Arc::default())
    }

    /// Fails if the OS keychain cannot be used on this machine.
    pub fn check_keyring() -> Result<(), String> {
        keyring::Entry::store_status()
            .as_ref()
            .map_err(ToString::to_string)
            .copied()
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>, ApiError> {
        let key = key.to_owned();
        self.run(move |store| match store {
            Self::Keyring => match keyring::Entry::new(SERVICE, &key)?.get_password() {
                Ok(v) => Ok(Some(v)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(e.into()),
            },
            Self::File(path, _) => Ok(read_file(path)?.remove(&key)),
            Self::Memory(map) => Ok(lock(map).get(&key).cloned()),
        })
        .await
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<(), ApiError> {
        let (key, value) = (key.to_owned(), value.to_owned());
        self.run(move |store| match store {
            Self::Keyring => Ok(keyring::Entry::new(SERVICE, &key)?.set_password(&value)?),
            Self::File(path, _) => {
                let mut all = read_file(path)?;
                all.insert(key, value);
                write_file(path, &all)
            }
            Self::Memory(map) => {
                lock(map).insert(key, value);
                Ok(())
            }
        })
        .await
    }

    /// A random 32-byte key kept under `key`, made on first use. A stored value that is not such
    /// a key is an error: replacing it would lose whatever the old key protects.
    pub async fn key32(&self, key: &str) -> Result<[u8; 32], String> {
        if let Some(hex) = self.get(key).await.map_err(|e| e.to_string())? {
            let mut bytes = [0; 32];
            hex::decode_to_slice(hex.trim(), &mut bytes)
                .map_err(|_| format!("{key} in the secret store is not a 32-byte key"))?;
            return Ok(bytes);
        }
        let mut bytes = [0; 32];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
            .map_err(|_| "no system randomness".to_owned())?;
        self.set(key, &hex::encode(bytes))
            .await
            .map_err(|e| e.to_string())?;
        Ok(bytes)
    }

    /// Removes a secret; a missing one is not an error.
    pub async fn delete(&self, key: &str) -> Result<(), ApiError> {
        let key = key.to_owned();
        self.run(move |store| match store {
            Self::Keyring => match keyring::Entry::new(SERVICE, &key)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(e.into()),
            },
            Self::File(path, _) => {
                let mut all = read_file(path)?;
                if all.remove(&key).is_some() {
                    write_file(path, &all)?;
                }
                Ok(())
            }
            Self::Memory(map) => {
                lock(map).remove(&key);
                Ok(())
            }
        })
        .await
    }

    /// Runs `f` on the blocking pool (keychains block), serialised for the file store.
    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Self) -> Result<T, Box<dyn std::error::Error + Send + Sync>> + Send + 'static,
    ) -> Result<T, ApiError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = match &store {
                Self::File(_, gate) => Some(gate.lock().unwrap_or_else(PoisonError::into_inner)),
                _ => None,
            };
            f(&store)
        })
        .await
        .map_err(internal)?
        .map_err(internal)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

type FileResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn read_file(path: &PathBuf) -> FileResult<BTreeMap<String, String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e.into()),
    }
}

fn write_file(path: &PathBuf, all: &BTreeMap<String, String>) -> FileResult<()> {
    let tmp = path.with_extension("tmp");
    // A planted tmp file (or symlink) would keep its own mode; start fresh.
    match std::fs::remove_file(&tmp) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&tmp)?, &serde_json::to_vec(all)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn roundtrip(store: &SecretStore) {
        assert_eq!(store.get("provider:a").await.unwrap(), None);
        store.set("provider:a", "sk-1").await.unwrap();
        store.set("provider:b", "sk-2").await.unwrap();
        assert_eq!(
            store.get("provider:a").await.unwrap().as_deref(),
            Some("sk-1")
        );
        store.delete("provider:a").await.unwrap();
        store.delete("provider:a").await.unwrap();
        assert_eq!(store.get("provider:a").await.unwrap(), None);
        assert_eq!(
            store.get("provider:b").await.unwrap().as_deref(),
            Some("sk-2")
        );
    }

    #[tokio::test]
    async fn memory_and_file_stores() {
        roundtrip(&SecretStore::default()).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // A world-readable leftover tmp file must not pass its mode on.
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, b"{}").unwrap();
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        roundtrip(&SecretStore::file(path.clone())).await;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn debug_never_shows_secrets() {
        let store = SecretStore::default();
        if let SecretStore::Memory(map) = &store {
            lock(map).insert("k".into(), "sk-secret".into());
        }
        assert_eq!(format!("{store:?}"), "SecretStore(memory)");
    }
}
