//! Fly.io Sprites backend (stub).
//!
//! A Sprite is a Firecracker microVM with a persistent ext4 disk that sleeps
//! on its own when idle and wakes on HTTP/exec. There is no official Rust
//! SDK, so this module carries a small REST client skeleton.
//!
//! Design notes for the real implementation (phase 3):
//! * Sprites have no templates/images/env at create time. Provisioning is
//!   "create, then `exec` a setup script, then `checkpoint`" (checkpoint v0).
//!   Secrets are written with the filesystem API, not passed as env.
//! * There is no explicit stop/resume. `stop` becomes "close connections and
//!   let it sleep"; `wake` becomes an HTTP request or exec against the sprite.
//! * TCP connections drop when a sprite sleeps, which is why the runner dials
//!   the daemon and reconnects instead of being driven over a held stdin.
//! * Long turns need the in-sprite Tasks API heartbeat to stay awake.
//!
//! Endpoints (from <https://sprites.dev/api>, checked 2026-11-19):
//! `POST /v1/sprites`, `DELETE /v1/sprites/{name}`,
//! `POST /v1/sprites/{name}/exec`, `GET /v1/sprites/{name}/fs/read`,
//! `PUT /v1/sprites/{name}/fs/write`, `POST /v1/sprites/{name}/checkpoint`.
//! Lifecycle: <https://docs.fly.io/sprites/concepts/lifecycle.md>.

use async_trait::async_trait;

use crate::{
    ExecOutput, ExecRequest, SandboxBackend, SandboxError, SandboxHandle, SandboxPath, SandboxSpec,
    SandboxState, SnapshotRef,
};

const KIND: &str = "sprites";

#[derive(Clone)]
pub struct SpritesConfig {
    pub base_url: String,
    /// Bearer token. Never logged (see the manual `Debug` impl).
    pub token: String,
}

impl std::fmt::Debug for SpritesConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpritesConfig")
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl Default for SpritesConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.sprites.dev".into(),
            token: String::new(),
        }
    }
}

/// Thin REST client skeleton. Request/response types will be generated or
/// hand-written from the published OpenAPI spec in phase 3.
#[derive(Debug, Clone)]
pub struct SpritesBackend {
    http: reqwest::Client,
    config: SpritesConfig,
}

impl SpritesBackend {
    pub fn new(config: SpritesConfig) -> Result<Self, SandboxError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ferrobot/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { http, config })
    }

    /// Builds an authenticated request for `path` (e.g. `/v1/sprites`).
    #[allow(dead_code)] // used once the operations below are implemented
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.config.base_url))
            .bearer_auth(&self.config.token)
    }

    fn todo(operation: &'static str) -> SandboxError {
        SandboxError::NotImplemented {
            backend: KIND,
            operation,
        }
    }
}

#[async_trait]
impl SandboxBackend for SpritesBackend {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn create(&self, _spec: &SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        // POST /v1/sprites {name}; then exec setup + fs/write secrets + checkpoint.
        Err(Self::todo("create"))
    }

    async fn exec(&self, _sandbox: &SandboxHandle, _req: &ExecRequest) -> Result<ExecOutput, SandboxError> {
        // POST /v1/sprites/{name}/exec (non-TTY HTTP variant).
        Err(Self::todo("exec"))
    }

    async fn read_file(&self, _sandbox: &SandboxHandle, _path: &SandboxPath) -> Result<Vec<u8>, SandboxError> {
        // GET /v1/sprites/{name}/fs/read
        Err(Self::todo("read_file"))
    }

    async fn write_file(&self, _sandbox: &SandboxHandle, _path: &SandboxPath, _contents: &[u8]) -> Result<(), SandboxError> {
        // PUT /v1/sprites/{name}/fs/write
        Err(Self::todo("write_file"))
    }

    async fn snapshot(&self, _sandbox: &SandboxHandle, _label: &str) -> Result<SnapshotRef, SandboxError> {
        // POST /v1/sprites/{name}/checkpoint (streams NDJSON progress)
        Err(Self::todo("snapshot"))
    }

    async fn wake(&self, _sandbox: &SandboxHandle) -> Result<(), SandboxError> {
        // No resume endpoint: any exec or request to the sprite URL wakes it.
        Err(Self::todo("wake"))
    }

    async fn stop(&self, _sandbox: &SandboxHandle) -> Result<(), SandboxError> {
        // No stop endpoint: sprites sleep by themselves when idle.
        Err(Self::todo("stop"))
    }

    async fn destroy(&self, _sandbox: &SandboxHandle, _purge_disk: bool) -> Result<(), SandboxError> {
        // DELETE /v1/sprites/{name}
        Err(Self::todo("destroy"))
    }

    async fn state(&self, _sandbox: &SandboxHandle) -> Result<SandboxState, SandboxError> {
        // GET /v1/sprites/{name}
        Err(Self::todo("state"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn every_operation_is_explicitly_not_implemented() {
        let b = SpritesBackend::new(SpritesConfig::default()).unwrap();
        let h = SandboxHandle {
            backend: KIND.into(),
            id: "x".into(),
        };
        let err = b.wake(&h).await.unwrap_err();
        assert!(matches!(err, SandboxError::NotImplemented { operation: "wake", .. }));
        assert!(err.to_string().contains("sprites backend does not implement wake"));
    }

    #[test]
    fn debug_redacts_token() {
        let c = SpritesConfig {
            token: "super-secret".into(),
            ..SpritesConfig::default()
        };
        assert!(!format!("{c:?}").contains("super-secret"));
    }
}
