//! Files agents share: copied into object storage (`artifacts/<agent>/<uuid>`) and shown as a
//! `file` event, so they outlive the sandbox and stay the version that was shared.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferrobot_core::AgentId;
use ferrobot_mcp::SharedFile;
use ferrobot_sandbox::{SandboxError, SandboxPath};
use futures_util::TryStreamExt;
use object_store::ObjectStoreExt;
use object_store::path::Path as Key;
use serde_json::json;

use crate::{AppState, UiEvent, lifecycle};

/// Largest file an agent can share.
pub const MAX_SHARE_BYTES: usize = 10 * 1024 * 1024;

/// How long a file link works; the app asks for a fresh one when opening.
pub const LINK_TTL: Duration = Duration::from_secs(30 * 60);

/// Where the key that signs file links lives in the secret store.
const LINK_KEY: &str = "artifact-link-key";

/// Largest HTML block from the chat the daemon serves as a page.
pub const MAX_BLOCK_BYTES: usize = 512 * 1024;

/// How many blocks stay served; older links stop working.
const KEPT_BLOCKS: usize = 32;

/// HTML blocks from the chat, served as pages so their styles and scripts run under the shared
/// pages' policy rather than the app's. In memory: after a restart the app asks again.
#[derive(Debug, Default)]
pub struct Previews(Mutex<VecDeque<(String, Arc<str>)>>);

impl Previews {
    /// Keeps `html` under its SHA-256, so a block rendered again is kept once.
    pub fn put(&self, html: &str) -> String {
        let id = hex::encode(ring::digest::digest(&ring::digest::SHA256, html.as_bytes()));
        let mut kept = crate::lock(&self.0);
        kept.retain(|(k, _)| *k != id);
        if kept.len() >= KEPT_BLOCKS {
            kept.pop_front();
        }
        kept.push_back((id.clone(), html.into()));
        id
    }

    pub fn get(&self, id: &str) -> Option<Arc<str>> {
        let kept = crate::lock(&self.0);
        kept.iter().find(|(k, _)| k == id).map(|(_, h)| h.clone())
    }
}

/// A file named by an agent: absolute, below its home (`~/x`) or, like its
/// tools see it, relative to its workspace.
pub fn resolve(home: &str, raw: &str) -> String {
    let raw = raw.trim();
    if raw.starts_with('/') {
        raw.to_owned()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        format!("{home}/workspace/{}", raw.trim_start_matches("./"))
    }
}

fn prefix(agent: AgentId) -> Key {
    Key::from(format!("artifacts/{agent}"))
}

/// Copies a file from the agent's computer into storage and shows it in its
/// chat. `Err` is a message for the agent.
pub async fn share(state: &AppState, agent: AgentId, raw: &str) -> Result<SharedFile, String> {
    let record = state
        .store
        .get_agent(agent)
        .await
        .map_err(|e| e.to_string())?;
    let handle = lifecycle::handle_of(&record).ok_or("your computer is not set up")?;
    let path =
        SandboxPath::parse(&resolve(&state.backend.home(), raw)).map_err(|e| e.to_string())?;
    let bytes = match state.backend.read_file(&handle, &path).await {
        Ok(b) if b.len() <= MAX_SHARE_BYTES => b,
        Ok(_) | Err(SandboxError::TooLarge(_)) => {
            return Err(format!("{path} is larger than {MAX_SHARE_BYTES} bytes"));
        }
        Err(SandboxError::NotFound(_)) => return Err(format!("there is no file at {path}")),
        Err(e) => {
            tracing::warn!(%agent, error = %e, "cannot read a shared file");
            return Err(format!("cannot read {path}"));
        }
    };
    let key = prefix(agent).join(uuid::Uuid::new_v4().to_string());
    let size = bytes.len();
    state
        .settings
        .storage
        .put(&key, bytes.into())
        .await
        .map_err(|e| {
            tracing::error!(%agent, error = %e, "cannot store a shared file");
            "the file could not be stored".to_owned()
        })?;
    let name = path.file_name().to_owned();
    let payload = json!({ "name": name, "size": size, "path": path.as_str(), "key": key.as_ref() });
    let event = state
        .store
        .append_event(agent, None, "file".into(), payload)
        .await
        .map_err(|e| e.to_string())?;
    let id = event.id;
    state.publish(UiEvent::Event { event });
    Ok(SharedFile {
        id,
        name,
        size: size as u64,
    })
}

/// The stored copy of a file `agent` shared. Only keys `artifacts/<agent>/<uuid>`
/// are read: no other agent's files, no other part of the store.
pub async fn read(state: &AppState, agent: AgentId, key: &str) -> object_store::Result<Vec<u8>> {
    let own = key
        .strip_prefix(&format!("artifacts/{agent}/"))
        .is_some_and(|rest| uuid::Uuid::try_parse(rest).is_ok_and(|u| u.to_string() == rest));
    if !own {
        return Err(object_store::Error::NotFound {
            path: key.to_owned(),
            source: "not a file of this agent".into(),
        });
    }
    let bytes = state
        .settings
        .storage
        .get(&Key::from(key))
        .await?
        .bytes()
        .await?;
    Ok(bytes.to_vec())
}

/// The key signing file links, created on first use and kept in the
/// secret store (so links survive a daemon restart until they expire).
async fn link_key(state: &AppState) -> Result<ring::hmac::Key, String> {
    state
        .link_key
        .get_or_try_init(|| async {
            let bytes = state.settings.secrets.key32(LINK_KEY).await?;
            Ok::<_, String>(ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &bytes))
        })
        .await
        .cloned()
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

async fn sign(state: &AppState, message: &str) -> Result<String, String> {
    let key = link_key(state).await?;
    Ok(hex::encode(
        ring::hmac::sign(&key, message.as_bytes()).as_ref(),
    ))
}

async fn verify(state: &AppState, message: &str, expires: u64, sig: &str) -> bool {
    let (Ok(key), Ok(sig)) = (link_key(state).await, hex::decode(sig)) else {
        return false;
    };
    expires >= unix_now() && ring::hmac::verify(&key, message.as_bytes(), &sig).is_ok()
}

/// A link to a shared file that works until `expires` (unix seconds).
pub async fn signed_link(
    state: &AppState,
    agent: AgentId,
    file: i64,
    expires: u64,
) -> Result<String, String> {
    let sig = sign(state, &format!("{agent}/{file}/{expires}")).await?;
    Ok(format!("/artifacts/{agent}/{file}?exp={expires}&sig={sig}"))
}

/// A link to a shared file that works for [`LINK_TTL`].
pub async fn fresh_link(state: &AppState, agent: AgentId, file: i64) -> Result<String, String> {
    signed_link(state, agent, file, unix_now() + LINK_TTL.as_secs()).await
}

/// Whether a link's signature is ours and it has not expired.
pub async fn link_valid(
    state: &AppState,
    agent: AgentId,
    file: i64,
    expires: u64,
    sig: &str,
) -> bool {
    verify(state, &format!("{agent}/{file}/{expires}"), expires, sig).await
}

/// A link to a kept HTML block (see [`Previews`]) until `expires` (unix seconds).
pub async fn preview_link(state: &AppState, id: &str, expires: u64) -> Result<String, String> {
    // Agent ids are UUIDs, so `preview/...` never signs like a file link.
    let sig = sign(state, &format!("preview/{id}/{expires}")).await?;
    Ok(format!("/artifacts/previews/{id}?exp={expires}&sig={sig}"))
}

/// A link to a kept HTML block that works for [`LINK_TTL`].
pub async fn fresh_preview_link(state: &AppState, id: &str) -> Result<String, String> {
    preview_link(state, id, unix_now() + LINK_TTL.as_secs()).await
}

pub async fn preview_link_valid(state: &AppState, id: &str, expires: u64, sig: &str) -> bool {
    verify(state, &format!("preview/{id}/{expires}"), expires, sig).await
}

/// Whether a shared file is a PDF by name (the content is checked too).
pub fn is_pdf(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Whether a shared file opens as a page.
pub fn is_page(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
}

/// Deletes every file the agent shared (the agent is gone for good).
pub async fn forget_agent(state: &AppState, agent: AgentId) {
    let storage = &state.settings.storage;
    let listed: object_store::Result<Vec<_>> =
        storage.list(Some(&prefix(agent))).try_collect().await;
    for meta in listed.unwrap_or_default() {
        if let Err(e) = storage.delete(&meta.location).await {
            tracing::warn!(%agent, error = %e, "cannot delete a shared file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_like_the_agents_tools() {
        assert_eq!(
            resolve("/home/node", "plan.md"),
            "/home/node/workspace/plan.md"
        );
        assert_eq!(
            resolve("/home/node", "./out/a.md"),
            "/home/node/workspace/out/a.md"
        );
        assert_eq!(
            resolve("/home/node", "~/notes/x.md"),
            "/home/node/notes/x.md"
        );
        assert_eq!(resolve("/home", "/tmp/x.csv"), "/tmp/x.csv");
    }
}
