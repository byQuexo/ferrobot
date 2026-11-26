//! "One computer per agent": the [`SandboxBackend`] trait and its backends ([`DockerBackend`],
//! [`FirecrackerBackend`], [`LocalProcessBackend`] without isolation, the [`SpritesBackend`] stub).

mod docker;
mod error;
mod firecracker;
mod local;
mod path;
pub mod seal;
mod sprites;

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use docker::{DockerBackend, DockerConfig};
pub use error::{MAX_FILE_BYTES, SandboxError};
pub use firecracker::{FirecrackerBackend, FirecrackerConfig};
pub use local::{LocalProcessBackend, LocalProcessConfig};
pub use path::SandboxPath;
pub use sprites::{SpritesBackend, SpritesConfig};

/// Upper bound for captured exec output per stream.
pub const MAX_EXEC_OUTPUT: usize = 1024 * 1024;

/// Most Debian packages [`SandboxBackend::build_image`] installs: the operator's CLIs plus the
/// desktop's.
pub const MAX_PACKAGES: usize = 48;

/// A running command's stdout as a stream and its stdin as a writer
/// ([`SandboxBackend::exec_stream`]).
pub struct ExecStream {
    pub output: futures_util::stream::BoxStream<'static, Result<bytes::Bytes, SandboxError>>,
    pub input: std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>,
}

impl std::fmt::Debug for ExecStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecStream").finish_non_exhaustive()
    }
}

/// Validates Debian package names (policy 5.6.7: lower case letters, digits, `+`, `-`, `.`,
/// alphanumeric first, at least two characters) and returns them sorted and deduplicated.
pub fn package_set(packages: &[String]) -> Result<Vec<String>, SandboxError> {
    let valid = |p: &str| {
        let mut chars = p.chars();
        (2..=63).contains(&p.len())
            && chars
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+.-".contains(c))
    };
    if let Some(bad) = packages.iter().find(|p| !valid(p)) {
        return Err(SandboxError::InvalidPackages(format!(
            "{bad:?} is not a Debian package name"
        )));
    }
    let mut set = packages.to_vec();
    set.sort();
    set.dedup();
    if set.len() > MAX_PACKAGES {
        return Err(SandboxError::InvalidPackages(format!(
            "at most {MAX_PACKAGES} packages"
        )));
    }
    Ok(set)
}

/// What to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSpec {
    /// DNS-safe unique name (the agent name). Used for container/volume names.
    pub name: String,
    /// Environment for the runner process. May contain secrets: never log.
    pub env: BTreeMap<String, String>,
    /// Free-form labels (agent id, project, ...).
    pub labels: BTreeMap<String, String>,
    /// Image from [`SandboxBackend::build_image`]; `None`: the backend's default image.
    pub image: Option<String>,
    /// Runs instead of the runner, e.g. to keep a helper container idle (Docker only).
    pub command: Option<Vec<String>>,
}

/// Reference to an existing sandbox. Persisted by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxHandle {
    /// Backend kind, e.g. `docker`.
    pub backend: String,
    /// Backend-specific id (container id, sprite name, directory name).
    pub id: String,
}

impl SandboxHandle {
    /// The id, if the handle belongs to backend `kind`.
    pub(crate) fn id_for(&self, kind: &'static str) -> Result<&str, SandboxError> {
        if self.backend == kind {
            Ok(&self.id)
        } else {
            Err(SandboxError::WrongBackend {
                expected: kind,
                actual: self.backend.clone(),
            })
        }
    }

    /// [`Self::id_for`] for backends whose ids are directory names.
    pub(crate) fn dir_name(&self, kind: &'static str) -> Result<&str, SandboxError> {
        let id = self.id_for(kind)?;
        // Refuse anything that could escape the backend's directory.
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(SandboxError::NotFound(id.to_owned()));
        }
        Ok(id)
    }
}

/// A sandbox's state as its backend reports it.
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
    /// Moved to object storage: nothing is left on the host. The next
    /// `create` for the same name restores it.
    Archived,
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

/// Exit code and captured output of a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct ExecOutput {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
    /// True if output was cut at [`MAX_EXEC_OUTPUT`].
    pub truncated: bool,
}

impl ExecOutput {
    /// Cuts each stream at [`MAX_EXEC_OUTPUT`] and decodes it lossily.
    pub(crate) fn captured(exit_code: i64, stdout: &[u8], stderr: &[u8]) -> Self {
        let cut =
            |b: &[u8]| String::from_utf8_lossy(&b[..b.len().min(MAX_EXEC_OUTPUT)]).into_owned();
        Self {
            exit_code,
            stdout: cut(stdout),
            stderr: cut(stderr),
            truncated: stdout.len().max(stderr.len()) > MAX_EXEC_OUTPUT,
        }
    }
}

/// A snapshot a backend took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SnapshotRef {
    pub id: String,
    /// What the snapshot covers, e.g. "container root filesystem and home directory".
    pub scope: String,
}

/// A backend that gives each agent its own computer. `create` on an existing name adopts it;
/// `wake` on a running sandbox and `stop` on a stopped one are no-ops.
#[async_trait]
pub trait SandboxBackend: Send + Sync + std::fmt::Debug {
    /// Stable backend kind, stored next to the sandbox id.
    fn kind(&self) -> &'static str;

    /// Whether [`snapshot`](Self::snapshot) and [`restore`](Self::restore) work.
    fn supports_snapshots(&self) -> bool {
        false
    }

    /// The runner's home directory as a [`SandboxPath`] prefix; its
    /// workspace is `<home>/workspace`.
    fn home(&self) -> String {
        "/home/node".into()
    }

    /// Whether [`build_image`](Self::build_image) can add packages.
    fn supports_image_builds(&self) -> bool {
        false
    }

    /// Builds the default image plus the Debian `packages` (see [`package_set`]) and returns
    /// it for [`SandboxSpec::image`]; no packages: `None`, the default image.
    async fn build_image(&self, packages: &[String]) -> Result<Option<String>, SandboxError> {
        if package_set(packages)?.is_empty() {
            return Ok(None);
        }
        Err(SandboxError::NotImplemented {
            backend: self.kind(),
            operation: "image builds",
        })
    }

    /// Whether the sandbox must be recreated to run `image` (`None`: the default image).
    /// Sandboxes restored from a snapshot or archive keep their own filesystem.
    async fn is_stale(
        &self,
        sandbox: &SandboxHandle,
        image: Option<&str>,
    ) -> Result<bool, SandboxError> {
        let _ = (sandbox, image);
        Ok(false)
    }

    /// Whether the backend can run sandboxes right now (e.g. the Docker engine answers).
    async fn ready(&self) -> Result<(), SandboxError> {
        Ok(())
    }

    /// The image sandboxes start from by default and its id if the engine has it pulled;
    /// `None`: the backend has no images.
    async fn default_image(&self) -> Option<(String, Option<String>)> {
        None
    }

    /// Creates (or adopts) and starts the sandbox. A sandbox archived with
    /// `sleep(Archived)` comes back from its archive.
    async fn create(&self, spec: &SandboxSpec) -> Result<SandboxHandle, SandboxError>;

    /// Runs a command to completion and captures its output.
    async fn exec(
        &self,
        sandbox: &SandboxHandle,
        req: &ExecRequest,
    ) -> Result<ExecOutput, SandboxError>;

    /// Starts `cmd` with its stdin and stdout piped to the caller, e.g. a VNC session; stderr
    /// is dropped. It runs until it exits or the input is closed.
    async fn exec_stream(
        &self,
        sandbox: &SandboxHandle,
        cmd: Vec<String>,
    ) -> Result<ExecStream, SandboxError> {
        let _ = (sandbox, cmd);
        Err(SandboxError::NotImplemented {
            backend: self.kind(),
            operation: "streaming exec",
        })
    }

    /// Reads a file of at most [`MAX_FILE_BYTES`] ([`SandboxError::TooLarge`]).
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

    /// Puts the sandbox to sleep and returns the level reached (`Cold` if it was not running).
    /// By default `Cold` is [`stop`](Self::stop) and `Warm`/`Archived` are `NotImplemented`.
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
            SleepLevel::Archived => Err(SandboxError::NotImplemented {
                backend: self.kind(),
                operation: "archive",
            }),
        }
    }

    /// Deletes the archive of sandbox `name`, if any.
    async fn delete_archive(&self, name: &str) -> Result<(), SandboxError> {
        let _ = name;
        Ok(())
    }

    /// Stops the sandbox, keeping its disk.
    async fn stop(&self, sandbox: &SandboxHandle) -> Result<(), SandboxError>;

    /// Removes the sandbox. The persistent disk is kept unless `purge_disk`.
    async fn destroy(&self, sandbox: &SandboxHandle, purge_disk: bool) -> Result<(), SandboxError>;

    async fn state(&self, sandbox: &SandboxHandle) -> Result<SandboxState, SandboxError>;
}

/// `Ok` also when there was nothing to remove.
pub(crate) fn removed(result: std::io::Result<()>) -> Result<(), SandboxError> {
    match result {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PACKAGES, package_set};

    #[test]
    fn package_names_follow_debian_policy() {
        for ok in [
            "jq",
            "poppler-utils",
            "g++",
            "libstdc++6",
            "python3.11",
            "7zip",
        ] {
            assert!(package_set(&[ok.into()]).is_ok(), "{ok}");
        }
        let long = "a".repeat(64);
        for bad in [
            "", "a", "-o", "+x", ".x", "Jq", "jq;id", "jq bar", "jq\n", "jq=1.6", "a/b", "$(id)",
            "jq*", "\"jq\"", &long,
        ] {
            assert!(package_set(&[bad.into()]).is_err(), "{bad:?}");
        }
        let set = package_set(&["tree".into(), "jq".into(), "tree".into()]).unwrap();
        assert_eq!(set, ["jq", "tree"]);
        let many: Vec<String> = (0..=MAX_PACKAGES).map(|i| format!("p{i}")).collect();
        assert!(package_set(&many).is_err());
        assert!(package_set(&many[1..]).is_ok());
    }
}
