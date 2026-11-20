//! Object storage for blobs that live outside SQLite and the sandbox disks:
//! snapshot archives today, later artifacts and archived sandboxes.

use std::path::Path;
use std::sync::Arc;

use ferrobot_sandbox::seal::SealKey;
use object_store::ObjectStore;
use object_store::local::LocalFileSystem;
use object_store::prefix::PrefixStore;

use crate::secrets::SecretStore;

/// Secret store entry of the key archives are encrypted with.
const ARCHIVE_KEY: &str = "archive-key";

/// Whether `--storage-url` keeps blobs on this machine.
pub fn is_local(url: Option<&str>) -> bool {
    url.is_none_or(|u| u.starts_with("file://") || u.starts_with("memory://"))
}

/// The key archives are encrypted with. Without a usable secret store a
/// local store gets plaintext archives; a remote one is refused.
pub async fn archive_key(
    url: Option<&str>,
    secrets: &SecretStore,
) -> Result<Option<SealKey>, String> {
    match secrets.key32(ARCHIVE_KEY).await {
        Ok(key) => Ok(Some(SealKey::new(&key))),
        Err(e) if is_local(url) => {
            tracing::warn!(error = %e, "no archive key: archives are stored unencrypted");
            Ok(None)
        }
        Err(e) => Err(format!(
            "archives in remote storage must be encrypted, but the key is unavailable: {e} \
             (use --secret-store file on hosts without a keychain)"
        )),
    }
}

/// Opens the store at `url`, or the data directory when `url` is `None`.
///
/// `s3://bucket/prefix` reads credentials, region and endpoint from the
/// standard `AWS_*` variables (Cloudflare R2: `AWS_ENDPOINT` plus
/// `AWS_REGION=auto`); `file:///abs/dir` and `memory://` work as well.
pub fn open(url: Option<&str>, data_dir: &Path) -> object_store::Result<Arc<dyn ObjectStore>> {
    let Some(url) = url else {
        // Keys like `snapshots/<tag>.tar` keep the layout of older releases.
        let local = LocalFileSystem::new_with_prefix(data_dir)?.with_automatic_cleanup(true);
        return Ok(Arc::new(local));
    };
    let parsed = url::Url::parse(url).map_err(|e| object_store::Error::Generic {
        store: "storage-url",
        source: format!("{url:?}: {e}").into(),
    })?;
    let aws_env = std::env::vars().filter(|(key, _)| key.starts_with("AWS_"));
    let (store, prefix) = object_store::parse_url_opts(&parsed, aws_env)?;
    Ok(Arc::new(PrefixStore::new(store, prefix)))
}

#[cfg(test)]
mod tests {
    use object_store::ObjectStoreExt;
    use object_store::path::Path as Key;

    use super::*;

    async fn roundtrip(store: &dyn ObjectStore) {
        let key = Key::from("snapshots/a-1.tar");
        store.put(&key, b"tar bytes".to_vec().into()).await.unwrap();
        let got = store.get(&key).await.unwrap().bytes().await.unwrap();
        assert_eq!(&got[..], b"tar bytes");
        store.delete(&key).await.unwrap();
        assert!(matches!(
            store.get(&key).await,
            Err(object_store::Error::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn defaults_to_the_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(None, dir.path()).unwrap();
        let key = Key::from("snapshots/old-1.tar");
        store.put(&key, b"x".to_vec().into()).await.unwrap();
        assert!(dir.path().join("snapshots/old-1.tar").exists());
        store.delete(&key).await.unwrap();
        roundtrip(store.as_ref()).await;
    }

    #[tokio::test]
    async fn other_backends_are_a_url_change() {
        let dir = tempfile::tempdir().unwrap();
        let file_url = format!("file://{}/blobs", dir.path().display());
        roundtrip(open(Some(&file_url), dir.path()).unwrap().as_ref()).await;
        roundtrip(open(Some("memory://"), dir.path()).unwrap().as_ref()).await;
        // Builds offline; requests would go to the bucket.
        open(Some("s3://bucket/ferrobot"), dir.path()).unwrap();
    }

    #[tokio::test]
    async fn remote_storage_needs_the_archive_key() {
        let secrets = SecretStore::default();
        let key = archive_key(Some("s3://b/p"), &secrets).await.unwrap();
        assert!(key.is_some());
        assert_eq!(
            secrets.key32(ARCHIVE_KEY).await.unwrap(),
            secrets.key32(ARCHIVE_KEY).await.unwrap(),
            "made once, then kept"
        );

        let dir = tempfile::tempdir().unwrap();
        let broken = SecretStore::file(dir.path().to_path_buf());
        assert!(archive_key(Some("s3://b/p"), &broken).await.is_err());
        assert!(archive_key(None, &broken).await.unwrap().is_none());
        assert!(is_local(Some("file:///x")) && is_local(Some("memory://")));
        assert!(!is_local(Some("s3://b/p")));

        let corrupt = SecretStore::default();
        corrupt.set(ARCHIVE_KEY, "not hex").await.unwrap();
        assert!(corrupt.key32(ARCHIVE_KEY).await.is_err(), "never replaced");
    }

    #[test]
    fn rejects_unknown_urls() {
        let dir = std::env::temp_dir();
        assert!(open(Some("ftp://host/x"), &dir).is_err());
        assert!(open(Some("not a url"), &dir).is_err());
    }
}
