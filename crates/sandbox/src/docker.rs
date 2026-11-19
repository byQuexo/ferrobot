//! Docker / Podman backend: one container plus one named volume per agent.

use std::collections::HashMap;
use std::io::Read;

use async_trait::async_trait;
use bollard::Docker;
use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, ExecConfig, HostConfig};
use bollard::query_parameters::{
    CommitContainerOptionsBuilder, CreateContainerOptionsBuilder, DownloadFromContainerOptionsBuilder,
    InspectContainerOptions, RemoveContainerOptionsBuilder, RemoveVolumeOptions,
    StopContainerOptionsBuilder, UploadToContainerOptionsBuilder,
};
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt};

use crate::{
    ExecOutput, ExecRequest, SandboxBackend, SandboxError, SandboxHandle, SandboxPath, SandboxSpec,
    SandboxState, SnapshotRef, capture,
};

const KIND: &str = "docker";
const LABEL_MANAGED: &str = "dev.ferrobot.managed";

/// Configuration for [`DockerBackend`].
#[derive(Debug, Clone)]
pub struct DockerConfig {
    /// Runner image, built from `runner/Dockerfile`.
    pub image: String,
    /// Prefix for container and volume names.
    pub name_prefix: String,
    /// Mount point of the persistent per-agent volume (the runner's `$HOME`).
    pub home_dir: String,
    pub memory_bytes: i64,
    pub pids_limit: i64,
    /// Repository for snapshot images created by `snapshot`.
    pub snapshot_repo: String,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            image: "ferrobot-runner:dev".into(),
            name_prefix: "ferrobot-".into(),
            home_dir: "/home/node".into(),
            memory_bytes: 2 * 1024 * 1024 * 1024,
            pids_limit: 512,
            snapshot_repo: "ferrobot-snapshot".into(),
        }
    }
}

/// One hardened container per agent.
///
/// * all capabilities dropped, `no-new-privileges`, memory + pid limits
/// * runs as the image's non-root user
/// * `$HOME` lives on a named volume, so `stop`/`destroy` keep agent state
/// * `host.docker.internal` resolves to the host so the runner can dial the daemon
#[derive(Debug, Clone)]
pub struct DockerBackend {
    docker: Docker,
    config: DockerConfig,
}

impl DockerBackend {
    /// Connects using `DOCKER_HOST` or the platform default socket.
    pub fn connect(config: DockerConfig) -> Result<Self, SandboxError> {
        Ok(Self {
            docker: Docker::connect_with_local_defaults()?,
            config,
        })
    }

    /// Fails fast if the engine is unreachable.
    pub async fn ping(&self) -> Result<(), SandboxError> {
        self.docker.ping().await?;
        Ok(())
    }

    fn container_name(&self, name: &str) -> String {
        format!("{}{name}", self.config.name_prefix)
    }

    fn volume_name(&self, name: &str) -> String {
        format!("{}{name}-home", self.config.name_prefix)
    }

    fn check(handle: &SandboxHandle) -> Result<&str, SandboxError> {
        if handle.backend == KIND {
            Ok(&handle.id)
        } else {
            Err(SandboxError::WrongBackend {
                expected: KIND,
                actual: handle.backend.clone(),
            })
        }
    }

    async fn ensure_started(&self, id: &str) -> Result<(), SandboxError> {
        let info = self
            .docker
            .inspect_container(id, None::<InspectContainerOptions>)
            .await
            .map_err(|e| not_found_or(e, id))?;
        let running = info.state.and_then(|s| s.running).unwrap_or(false);
        if !running {
            self.docker.start_container(id, None).await?;
        }
        Ok(())
    }
}

fn not_found_or(err: bollard::errors::Error, id: &str) -> SandboxError {
    match err {
        bollard::errors::Error::DockerResponseServerError { status_code: 404, .. } => {
            SandboxError::NotFound(id.to_owned())
        }
        other => other.into(),
    }
}

/// Builds a single-file tar archive for `upload_to_container`.
fn single_file_tar(name: &str, contents: &[u8]) -> Result<Vec<u8>, SandboxError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o644);
    header.set_uid(1000);
    header.set_gid(1000);
    header.set_mtime(
        time::OffsetDateTime::now_utc()
            .unix_timestamp()
            .try_into()
            .unwrap_or_default(),
    );
    let mut builder = tar::Builder::new(Vec::new());
    builder.append_data(&mut header, name, contents)?;
    Ok(builder.into_inner()?)
}

/// Extracts the first regular file from a tar archive.
fn first_file_from_tar(archive: &[u8], path: &str) -> Result<Vec<u8>, SandboxError> {
    let mut ar = tar::Archive::new(archive);
    for entry in ar.entries()? {
        let mut entry = entry?;
        if entry.header().entry_type().is_file() {
            let mut out = Vec::new();
            entry.read_to_end(&mut out)?;
            return Ok(out);
        }
    }
    Err(SandboxError::Other(format!("{path} is not a regular file")))
}

#[async_trait]
impl SandboxBackend for DockerBackend {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn create(&self, spec: &SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        let name = self.container_name(&spec.name);
        let mut labels: HashMap<String, String> = spec.labels.clone().into_iter().collect();
        labels.insert(LABEL_MANAGED.into(), "true".into());

        let body = ContainerCreateBody {
            image: Some(self.config.image.clone()),
            env: Some(spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
            labels: Some(labels),
            host_config: Some(HostConfig {
                binds: Some(vec![format!(
                    "{}:{}",
                    self.volume_name(&spec.name),
                    self.config.home_dir
                )]),
                extra_hosts: Some(vec!["host.docker.internal:host-gateway".into()]),
                cap_drop: Some(vec!["ALL".into()]),
                security_opt: Some(vec!["no-new-privileges".into()]),
                memory: Some(self.config.memory_bytes),
                pids_limit: Some(self.config.pids_limit),
                init: Some(true),
                ..HostConfig::default()
            }),
            ..ContainerCreateBody::default()
        };

        let options = CreateContainerOptionsBuilder::default().name(&name).build();
        let id = match self.docker.create_container(Some(options), body).await {
            Ok(created) => created.id,
            // Adopt an existing container with the same name (idempotent create).
            Err(bollard::errors::Error::DockerResponseServerError { status_code: 409, .. }) => {
                tracing::info!(container = %name, "adopting existing container");
                self.docker
                    .inspect_container(&name, None::<InspectContainerOptions>)
                    .await?
                    .id
                    .ok_or_else(|| SandboxError::Other("container without id".into()))?
            }
            Err(e) => return Err(e.into()),
        };
        self.ensure_started(&id).await?;
        Ok(SandboxHandle {
            backend: KIND.into(),
            id,
        })
    }

    async fn exec(&self, sandbox: &SandboxHandle, req: &ExecRequest) -> Result<ExecOutput, SandboxError> {
        let id = Self::check(sandbox)?;
        let exec = self
            .docker
            .create_exec(
                id,
                ExecConfig {
                    cmd: Some(req.cmd.clone()),
                    working_dir: req.workdir.clone(),
                    env: Some(req.env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..ExecConfig::default()
                },
            )
            .await
            .map_err(|e| not_found_or(e, id))?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let StartExecResults::Attached { mut output, .. } = self
            .docker
            .start_exec(&exec.id, None::<StartExecOptions>)
            .await?
        {
            while let Some(chunk) = output.next().await {
                match chunk? {
                    bollard::container::LogOutput::StdErr { message } => {
                        if stderr.len() <= crate::MAX_EXEC_OUTPUT {
                            stderr.extend_from_slice(&message);
                        }
                    }
                    other => {
                        if stdout.len() <= crate::MAX_EXEC_OUTPUT {
                            stdout.extend_from_slice(&other.into_bytes());
                        }
                    }
                }
            }
        }
        let exit_code = self
            .docker
            .inspect_exec(&exec.id)
            .await?
            .exit_code
            .unwrap_or(-1);
        let (stdout, t1) = capture(&stdout);
        let (stderr, t2) = capture(&stderr);
        Ok(ExecOutput {
            exit_code,
            stdout,
            stderr,
            truncated: t1 || t2,
        })
    }

    async fn read_file(&self, sandbox: &SandboxHandle, path: &SandboxPath) -> Result<Vec<u8>, SandboxError> {
        let id = Self::check(sandbox)?;
        let options = DownloadFromContainerOptionsBuilder::default()
            .path(path.as_str())
            .build();
        let chunks: Vec<Bytes> = self
            .docker
            .download_from_container(id, Some(options))
            .try_collect()
            .await
            .map_err(|e| not_found_or(e, path.as_str()))?;
        first_file_from_tar(&chunks.concat(), path.as_str())
    }

    async fn write_file(&self, sandbox: &SandboxHandle, path: &SandboxPath, contents: &[u8]) -> Result<(), SandboxError> {
        let id = Self::check(sandbox)?;
        let archive = single_file_tar(path.file_name(), contents)?;
        let options = UploadToContainerOptionsBuilder::default()
            .path(path.parent())
            .build();
        self.docker
            .upload_to_container(id, Some(options), bollard::body_full(archive.into()))
            .await
            .map_err(|e| not_found_or(e, path.parent()))?;
        Ok(())
    }

    async fn snapshot(&self, sandbox: &SandboxHandle, label: &str) -> Result<SnapshotRef, SandboxError> {
        let id = Self::check(sandbox)?;
        let tag = format!(
            "{}-{}",
            label
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
                .collect::<String>(),
            time::OffsetDateTime::now_utc().unix_timestamp()
        );
        let options = CommitContainerOptionsBuilder::default()
            .container(id)
            .repo(&self.config.snapshot_repo)
            .tag(&tag)
            .comment("ferrobot snapshot")
            .build();
        self.docker
            .commit_container(options, bollard::models::ContainerConfig::default())
            .await
            .map_err(|e| not_found_or(e, id))?;
        Ok(SnapshotRef {
            id: format!("{}:{tag}", self.config.snapshot_repo),
            scope: "container root filesystem; the home volume is not included".into(),
        })
    }

    async fn wake(&self, sandbox: &SandboxHandle) -> Result<(), SandboxError> {
        self.ensure_started(Self::check(sandbox)?).await
    }

    async fn stop(&self, sandbox: &SandboxHandle) -> Result<(), SandboxError> {
        let id = Self::check(sandbox)?;
        let options = StopContainerOptionsBuilder::default().t(10).build();
        match self.docker.stop_container(id, Some(options)).await {
            // 304: already stopped
            Ok(()) | Err(bollard::errors::Error::DockerResponseServerError { status_code: 304, .. }) => Ok(()),
            Err(e) => Err(not_found_or(e, id)),
        }
    }

    async fn destroy(&self, sandbox: &SandboxHandle, purge_disk: bool) -> Result<(), SandboxError> {
        let id = Self::check(sandbox)?;
        // Resolve the volume before the container disappears.
        let volume = if purge_disk {
            self.docker
                .inspect_container(id, None::<InspectContainerOptions>)
                .await
                .ok()
                .and_then(|info| info.mounts)
                .and_then(|mounts| mounts.into_iter().find_map(|m| m.name))
        } else {
            None
        };
        let options = RemoveContainerOptionsBuilder::default().force(true).build();
        match self.docker.remove_container(id, Some(options)).await {
            Ok(()) | Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. }) => {}
            Err(e) => return Err(e.into()),
        }
        if let Some(volume) = volume {
            self.docker
                .remove_volume(&volume, None::<RemoveVolumeOptions>)
                .await?;
        }
        Ok(())
    }

    async fn state(&self, sandbox: &SandboxHandle) -> Result<SandboxState, SandboxError> {
        let id = Self::check(sandbox)?;
        match self
            .docker
            .inspect_container(id, None::<InspectContainerOptions>)
            .await
        {
            Ok(info) => Ok(if info.state.and_then(|s| s.running).unwrap_or(false) {
                SandboxState::Running
            } else {
                SandboxState::Stopped
            }),
            Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. }) => {
                Ok(SandboxState::Missing)
            }
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tar_roundtrip() {
        let archive = single_file_tar("hello.txt", b"hi there").unwrap();
        assert_eq!(first_file_from_tar(&archive, "/x/hello.txt").unwrap(), b"hi there");
    }

    #[test]
    fn rejects_foreign_handles() {
        let h = SandboxHandle {
            backend: "local".into(),
            id: "x".into(),
        };
        assert!(matches!(
            DockerBackend::check(&h),
            Err(SandboxError::WrongBackend { .. })
        ));
    }
}
