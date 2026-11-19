//! Docker / Podman backend: one container plus one named volume per agent.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use async_trait::async_trait;
use bollard::Docker;
use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, ExecConfig, HostConfig};
use bollard::query_parameters::{
    CommitContainerOptionsBuilder, CreateContainerOptionsBuilder,
    DownloadFromContainerOptionsBuilder, InspectContainerOptions, RemoveContainerOptionsBuilder,
    RemoveImageOptionsBuilder, RemoveVolumeOptions, StopContainerOptionsBuilder,
    UploadToContainerOptionsBuilder,
};
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt};
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt};

use crate::{
    ExecOutput, ExecRequest, SandboxBackend, SandboxError, SandboxHandle, SandboxPath, SandboxSpec,
    SandboxState, SleepLevel, SnapshotRef, capture,
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
    /// Where `snapshot` stores archives of the home volume (which `docker
    /// commit` does not capture), as `snapshots/<tag>.tar`. `None`: snapshots
    /// cover the root filesystem only.
    pub snapshot_store: Option<Arc<dyn ObjectStore>>,
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
            snapshot_store: None,
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

    /// Store and key of the home archive for a snapshot image reference (`repo:tag`).
    fn archive(&self, snapshot: &str) -> Option<(&dyn ObjectStore, ObjectPath)> {
        let tag = snapshot.rsplit_once(':').map_or(snapshot, |(_, tag)| tag);
        let safe = tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
        if !safe || tag.starts_with('.') {
            return None;
        }
        let store = self.config.snapshot_store.as_deref()?;
        Some((store, ObjectPath::from(format!("snapshots/{tag}.tar"))))
    }

    fn check_snapshot<'a>(&self, snapshot: &'a str) -> Result<&'a str, SandboxError> {
        match snapshot.split_once(':') {
            Some((repo, tag)) if repo == self.config.snapshot_repo && !tag.is_empty() => {
                Ok(snapshot)
            }
            _ => Err(SandboxError::NotFound(snapshot.to_owned())),
        }
    }

    /// Creates (or adopts) the container for `spec` from `image` without starting it.
    async fn create_container(
        &self,
        spec: &SandboxSpec,
        image: &str,
    ) -> Result<String, SandboxError> {
        let name = self.container_name(&spec.name);
        let mut labels: HashMap<String, String> = spec.labels.clone().into_iter().collect();
        labels.insert(LABEL_MANAGED.into(), "true".into());

        let body = ContainerCreateBody {
            image: Some(image.to_owned()),
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
        match self.docker.create_container(Some(options), body).await {
            Ok(created) => Ok(created.id),
            // Adopt an existing container with the same name (idempotent create).
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 409, ..
            }) => {
                tracing::info!(container = %name, "adopting existing container");
                self.docker
                    .inspect_container(&name, None::<InspectContainerOptions>)
                    .await?
                    .id
                    .ok_or_else(|| SandboxError::Other("container without id".into()))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn ensure_started(&self, id: &str) -> Result<(), SandboxError> {
        let info = self
            .docker
            .inspect_container(id, None::<InspectContainerOptions>)
            .await
            .map_err(|e| not_found_or(e, id))?;
        let state = info.state.unwrap_or_default();
        if state.paused == Some(true) {
            self.docker.unpause_container(id).await?;
        } else if state.running != Some(true) {
            self.docker.start_container(id, None).await?;
        }
        Ok(())
    }
}

fn not_found_or(err: bollard::errors::Error, id: &str) -> SandboxError {
    match err {
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        } => SandboxError::NotFound(id.to_owned()),
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

    fn supports_snapshots(&self) -> bool {
        true
    }

    async fn create(&self, spec: &SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        let id = self.create_container(spec, &self.config.image).await?;
        self.ensure_started(&id).await?;
        Ok(SandboxHandle {
            backend: KIND.into(),
            id,
        })
    }

    async fn exec(
        &self,
        sandbox: &SandboxHandle,
        req: &ExecRequest,
    ) -> Result<ExecOutput, SandboxError> {
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

    async fn read_file(
        &self,
        sandbox: &SandboxHandle,
        path: &SandboxPath,
    ) -> Result<Vec<u8>, SandboxError> {
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

    async fn write_file(
        &self,
        sandbox: &SandboxHandle,
        path: &SandboxPath,
        contents: &[u8],
    ) -> Result<(), SandboxError> {
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

    async fn snapshot(
        &self,
        sandbox: &SandboxHandle,
        label: &str,
    ) -> Result<SnapshotRef, SandboxError> {
        let id = Self::check(sandbox)?;
        let tag = format!(
            "{}-{}",
            label
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                })
                .collect::<String>(),
            time::OffsetDateTime::now_utc().unix_timestamp()
        );
        let image = format!("{}:{tag}", self.config.snapshot_repo);
        let archive = self.archive(&image);
        if let Some((store, key)) = &archive {
            let options = DownloadFromContainerOptionsBuilder::default()
                .path(&self.config.home_dir)
                .build();
            let chunks: Vec<Bytes> = self
                .docker
                .download_from_container(id, Some(options))
                .try_collect()
                .await
                .map_err(|e| not_found_or(e, id))?;
            // shortcut: the whole home archive is buffered and sent as one PUT (S3 caps
            // that at 5 GiB); switch to `put_multipart` once homes get that large.
            store.put(key, chunks.concat().into()).await?;
        }
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
        let scope = if archive.is_some() {
            "container root filesystem and home directory"
        } else {
            "container root filesystem; the home volume is not included"
        };
        Ok(SnapshotRef {
            id: image,
            scope: scope.into(),
        })
    }

    async fn restore(
        &self,
        sandbox: &SandboxHandle,
        snapshot: &str,
        spec: &SandboxSpec,
    ) -> Result<SandboxHandle, SandboxError> {
        let image = self.check_snapshot(snapshot)?;
        Self::check(sandbox)?;
        self.docker
            .inspect_image(image)
            .await
            .map_err(|e| not_found_or(e, image))?;
        let archive = match self.archive(image) {
            Some((store, key)) => match store.get(&key).await {
                Ok(found) => Some(found.bytes().await?),
                Err(object_store::Error::NotFound { .. }) => None,
                Err(e) => return Err(e.into()),
            },
            None => None,
        };
        // With a home archive the volume is recreated from it; without one the
        // current home volume is kept.
        self.destroy(sandbox, archive.is_some()).await?;
        let id = self.create_container(spec, image).await?;
        if let Some(archive) = archive {
            let parent = std::path::Path::new(&self.config.home_dir)
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or("/");
            let options = UploadToContainerOptionsBuilder::default()
                .path(parent)
                .copy_uidgid("true")
                .build();
            self.docker
                .upload_to_container(&id, Some(options), bollard::body_full(archive))
                .await?;
        }
        self.ensure_started(&id).await?;
        Ok(SandboxHandle {
            backend: KIND.into(),
            id,
        })
    }

    async fn delete_snapshot(&self, snapshot: &str) -> Result<(), SandboxError> {
        let image = self.check_snapshot(snapshot)?;
        let options = RemoveImageOptionsBuilder::default().force(true).build();
        match self.docker.remove_image(image, Some(options), None).await {
            Ok(_)
            | Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
            Err(e) => return Err(e.into()),
        }
        if let Some((store, key)) = self.archive(image) {
            match store.delete(&key).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    async fn wake(&self, sandbox: &SandboxHandle) -> Result<(), SandboxError> {
        self.ensure_started(Self::check(sandbox)?).await
    }

    async fn sleep(
        &self,
        sandbox: &SandboxHandle,
        level: SleepLevel,
    ) -> Result<SleepLevel, SandboxError> {
        if level == SleepLevel::Cold {
            self.stop(sandbox).await?;
            return Ok(level);
        }
        let id = Self::check(sandbox)?;
        match self.docker.pause_container(id).await {
            Ok(()) => Ok(level),
            // 409: already paused, or not running (then it already sleeps cold).
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 409, ..
            }) => Ok(match self.state(sandbox).await? {
                SandboxState::Paused => SleepLevel::Warm,
                _ => SleepLevel::Cold,
            }),
            Err(e) => Err(not_found_or(e, id)),
        }
    }

    async fn stop(&self, sandbox: &SandboxHandle) -> Result<(), SandboxError> {
        let id = Self::check(sandbox)?;
        // A paused container cannot handle SIGTERM; resume it so it stops cleanly.
        if self.state(sandbox).await? == SandboxState::Paused {
            self.docker.unpause_container(id).await?;
        }
        let options = StopContainerOptionsBuilder::default().t(10).build();
        match self.docker.stop_container(id, Some(options)).await {
            // 304: already stopped
            Ok(())
            | Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
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
            Ok(())
            | Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
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
            Ok(info) => {
                let state = info.state.unwrap_or_default();
                Ok(if state.paused == Some(true) {
                    SandboxState::Paused
                } else if state.running == Some(true) {
                    SandboxState::Running
                } else {
                    SandboxState::Stopped
                })
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(SandboxState::Missing),
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
        assert_eq!(
            first_file_from_tar(&archive, "/x/hello.txt").unwrap(),
            b"hi there"
        );
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
