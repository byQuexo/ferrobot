//! "One computer per agent": the [`SandboxBackend`] abstraction.
//!
//! A sandbox is an isolated machine (container, microVM, ...) owned by exactly
//! one agent. It has a persistent disk, can sleep and wake, and runs the
//! agent runner, which dials *out* to the daemon. Backends:
//!
//! * [`DockerBackend`]: one container per agent via the Docker Engine API
//!   (works with Podman's Docker-compatible socket too).
//! * [`LocalProcessBackend`]: runs the runner as a plain child process in a
//!   per-agent directory. **No isolation**; for development and CI only.
//! * [`SpritesBackend`]: Fly.io Sprites (Firecracker microVM per agent).
//!   Phase 0 ships the client skeleton only; every operation returns
//!   [`SandboxError::NotImplemented`].

mod docker;
mod error;
mod local;
mod path;
mod sprites;

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use docker::{DockerBackend, DockerConfig};
pub use error::SandboxError;
pub use local::{LocalProcessBackend, LocalProcessConfig};
pub use path::SandboxPath;
pub use sprites::{SpritesBackend, SpritesConfig};

/// Upper bound for captured exec output per stream.
pub const MAX_EXEC_OUTPUT: usize = 1024 * 1024;

/// What to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSpec {
    /// DNS-safe unique name (the agent name). Used for container/volume names.
    pub name: String,
    /// Environment for the runner process. May contain secrets: never log.
    pub env: BTreeMap<String, String>,
    /// Free-form labels (agent id, project, ...).
    pub labels: BTreeMap<String, String>,
}

/// Reference to an existing sandbox. Persisted by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxHandle {
    /// Backend kind, e.g. `docker`.
    pub backend: String,
    /// Backend-specific id (container id, sprite name, directory name).
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SandboxState {
    Running,
    /// Frozen by a warm sleep: memory kept, processes resume on wake.
    Paused,
    Stopped,
    Missing,
}

/// How deeply a sandbox sleeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SleepLevel {
    /// Paused: processes frozen, memory kept, wakes without a restart.
    Warm,
    /// Stopped: processes end and memory is freed; the disk is kept.
    Cold,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecRequest {
    /// argv; no shell is involved unless you pass one explicitly.
    pub cmd: Vec<String>,
    pub workdir: Option<String>,
    pub env: BTreeMap<String, String>,
}

impl ExecRequest {
    pub fn new<I, S>(cmd: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            cmd: cmd.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct ExecOutput {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
    /// True if output was cut at [`MAX_EXEC_OUTPUT`].
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SnapshotRef {
    pub id: String,
    /// What the snapshot covers, e.g. "container rootfs (home volume excluded)".
    pub scope: String,
}

/// A backend that gives each agent its own computer.
///
/// All operations are idempotent where that makes sense: `create` on an
/// existing name adopts it, `wake` on a running sandbox and `stop` on a
/// stopped one are no-ops.
#[async_trait]
pub trait SandboxBackend: Send + Sync + std::fmt::Debug {
    /// Stable backend kind, stored next to the sandbox id.
    fn kind(&self) -> &'static str;

    /// Whether [`snapshot`](Self::snapshot) and [`restore`](Self::restore) work.
    fn supports_snapshots(&self) -> bool {
        false
    }

    /// Creates (or adopts) and starts the sandbox.
    async fn create(&self, spec: &SandboxSpec) -> Result<SandboxHandle, SandboxError>;

    /// Runs a command to completion and captures its output.
    async fn exec(
        &self,
        sandbox: &SandboxHandle,
        req: &ExecRequest,
    ) -> Result<ExecOutput, SandboxError>;

    async fn read_file(
        &self,
        sandbox: &SandboxHandle,
        path: &SandboxPath,
    ) -> Result<Vec<u8>, SandboxError>;

    /// Writes a file; the parent directory must exist.
    async fn write_file(
        &self,
        sandbox: &SandboxHandle,
        path: &SandboxPath,
        contents: &[u8],
    ) -> Result<(), SandboxError>;

    /// Persists the current disk state under a new snapshot id.
    async fn snapshot(
        &self,
        sandbox: &SandboxHandle,
        label: &str,
    ) -> Result<SnapshotRef, SandboxError>;

    /// Replaces the sandbox with one booted from `snapshot` (an id returned by
    /// [`snapshot`](Self::snapshot)) and starts it. Returns the new handle.
    async fn restore(
        &self,
        sandbox: &SandboxHandle,
        snapshot: &str,
        spec: &SandboxSpec,
    ) -> Result<SandboxHandle, SandboxError> {
        let _ = (sandbox, snapshot, spec);
        Err(SandboxError::NotImplemented {
            backend: self.kind(),
            operation: "restore",
        })
    }

    /// Deletes a snapshot's data. Unknown snapshots are not an error.
    async fn delete_snapshot(&self, snapshot: &str) -> Result<(), SandboxError> {
        let _ = snapshot;
        Err(SandboxError::NotImplemented {
            backend: self.kind(),
            operation: "delete_snapshot",
        })
    }

    /// Starts a stopped/sleeping sandbox (resumes a paused one).
    async fn wake(&self, sandbox: &SandboxHandle) -> Result<(), SandboxError>;

    /// Puts the sandbox to sleep and returns the level reached (a sandbox
    /// that is not running reaches `Cold`). `Cold` is [`stop`](Self::stop);
    /// backends that cannot pause answer `Warm` with `NotImplemented`.
    async fn sleep(
        &self,
        sandbox: &SandboxHandle,
        level: SleepLevel,
    ) -> Result<SleepLevel, SandboxError> {
        match level {
            SleepLevel::Cold => {
                self.stop(sandbox).await?;
                Ok(level)
            }
            SleepLevel::Warm => Err(SandboxError::NotImplemented {
                backend: self.kind(),
                operation: "pause",
            }),
        }
    }

    /// Stops the sandbox, keeping its disk.
    async fn stop(&self, sandbox: &SandboxHandle) -> Result<(), SandboxError>;

    /// Removes the sandbox. The persistent disk is kept unless `purge_disk`.
    async fn destroy(&self, sandbox: &SandboxHandle, purge_disk: bool) -> Result<(), SandboxError>;

    async fn state(&self, sandbox: &SandboxHandle) -> Result<SandboxState, SandboxError>;
}

/// Truncates captured bytes to [`MAX_EXEC_OUTPUT`] and decodes lossily.
pub(crate) fn capture(bytes: &[u8]) -> (String, bool) {
    let truncated = bytes.len() > MAX_EXEC_OUTPUT;
    let slice = if truncated {
        &bytes[..MAX_EXEC_OUTPUT]
    } else {
        bytes
    };
    (String::from_utf8_lossy(slice).into_owned(), truncated)
}
