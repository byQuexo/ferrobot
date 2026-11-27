//! Docker / Podman backend: one container plus one named volume per agent.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use bollard::Docker;
use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, ContainerInspectResponse, ExecConfig, HostConfig};
use bollard::query_parameters::{
    BuildImageOptionsBuilder, CommitContainerOptionsBuilder, CreateContainerOptionsBuilder,
    CreateImageOptionsBuilder, DownloadFromContainerOptionsBuilder, ImportImageOptionsBuilder,
    InspectContainerOptions, ListImagesOptionsBuilder, RemoveContainerOptionsBuilder,
    RemoveImageOptions, RemoveImageOptionsBuilder, RemoveVolumeOptions,
    StopContainerOptionsBuilder, UploadToContainerOptionsBuilder,
};
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt};
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt};

use crate::seal::{self, Opener, SealKey, Sealer};
use crate::{
    ExecOutput, ExecRequest, MAX_FILE_BYTES, SandboxBackend, SandboxError, SandboxHandle,
    SandboxPath, SandboxSpec, SandboxState, SleepLevel, SnapshotRef,
};

const KIND: &str = "docker";
/// How often `prefetch_image` looks for an engine that is not up yet.
const PREFETCH_RETRY: std::time::Duration = std::time::Duration::from_secs(15);
const LABEL_MANAGED: &str = "dev.ferrobot.managed";
/// Image repository for archived sandboxes (separate from snapshots).
const ARCHIVE_REPO: &str = "ferrobot-archive";
const TOKEN_VAR: &str = "FERROBOT_AGENT_TOKEN";
/// Tag prefix of images from `build_image`; the sweep never touches any other image.
const CLI_TAG_PREFIX: &str = "ferrobot-runner:clis-";
/// Packages arrive as `"$@"`, never as part of the script.
/// Debian's `chromium` launcher reads `/etc/chromium.d`, so every way of starting it (dock, shell,
/// agent) gets the flags a container needs.
const INSTALL: &str = "export DEBIAN_FRONTEND=noninteractive && apt-get update \
    && apt-get install -y --no-install-recommends \"$@\" && rm -rf /var/lib/apt/lists/* \
    && if [ -d /etc/chromium.d ]; then printf '%s\\n' 'export CHROMIUM_FLAGS=\"$CHROMIUM_FLAGS \
    --no-sandbox --disable-dev-shm-usage --disable-gpu --no-first-run --no-default-browser-check \
    --password-store=basic --force-dark-mode --test-type\"' > /etc/chromium.d/ferrobot; fi";

/// Deterministic tag for `packages` (sorted) on the base image `base_id`, per daemon (`owner`), so
/// one daemon's sweep never removes another's image.
fn cli_tag(owner: &str, base_id: &str, packages: &[String]) -> String {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    // A changed recipe makes a new image, not a stale hit.
    ctx.update(INSTALL.as_bytes());
    ctx.update(base_id.as_bytes());
    for p in packages {
        ctx.update(b"\n");
        ctx.update(p.as_bytes());
    }
    ctx.finish().as_ref()[..8]
        .iter()
        .fold(format!("{CLI_TAG_PREFIX}{owner}-"), |mut tag, b| {
            let _ = write!(tag, "{b:02x}");
            tag
        })
}

/// Whether a sandbox from image `current` (name) should be recreated to run `wanted` (`None`:
/// `base`); snapshot and archive containers keep their root filesystem. `ids`: (the image id it
/// runs, the id `wanted` names now) when the names match, so a rebuilt image counts as new.
fn stale(current: &str, wanted: Option<&str>, base: &str, ids: Option<(&str, &str)>) -> bool {
    let built = current == base || current.starts_with(CLI_TAG_PREFIX);
    built && (current != wanted.unwrap_or(base) || ids.is_some_and(|(have, want)| have != want))
}

/// Dockerfile installing validated `packages` as root, then back to the base image's user.
fn dockerfile(base_id: &str, user: &str, packages: &[String]) -> Result<String, SandboxError> {
    let token = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
    };
    if !token(base_id) || !token(user) {
        return Err(SandboxError::Other(format!(
            "unexpected base image id {base_id:?} or user {user:?}"
        )));
    }
    let mut argv = vec!["sh", "-c", INSTALL, "sh"];
    argv.extend(packages.iter().map(String::as_str));
    let run = serde_json::to_string(&argv).map_err(|e| SandboxError::Other(e.to_string()))?;
    Ok(format!(
        "FROM {base_id}\nUSER root\nRUN {run}\nUSER {user}\n"
    ))
}

/// `ENV` change blanking every variable that holds the agent token: a committed image keeps the
/// container env, and the token also travels as e.g. `ANTHROPIC_API_KEY` or inside `HTTPS_PROXY`.
fn blank_token_env(env: &[String]) -> String {
    let token = env
        .iter()
        .find_map(|kv| kv.strip_prefix(TOKEN_VAR)?.strip_prefix('='))
        .filter(|t| !t.is_empty());
    let mut names = vec![TOKEN_VAR];
    names.extend(
        env.iter()
            .filter_map(|kv| kv.split_once('='))
            .filter(|(name, value)| *name != TOKEN_VAR && token.is_some_and(|t| value.contains(t)))
            .map(|(name, _)| name),
    );
    let pairs: Vec<String> = names.iter().map(|n| format!("{n}=")).collect();
    format!("ENV {}", pairs.join(" "))
}

/// Configuration for [`DockerBackend`].
#[derive(Debug, Clone)]
pub struct DockerConfig {
    /// Runner image, built from `runner/Dockerfile`.
    pub image: String,
    /// Prefix for container and volume names.
    pub name_prefix: String,
    /// Names this daemon's CLI images (lowercase letters and digits), so daemons sharing an
    /// engine leave each other's alone.
    pub owner: String,
    /// Mount point of the persistent per-agent volume (the runner's `$HOME`).
    pub home_dir: String,
    pub memory_bytes: i64,
    pub pids_limit: i64,
    /// Repository for snapshot images created by `snapshot`.
    pub snapshot_repo: String,
    /// Where `snapshot` stores the home volume, which `docker commit` skips, as
    /// `snapshots/<tag>.tar`. `None`: snapshots cover the root filesystem only.
    pub snapshot_store: Option<Arc<dyn ObjectStore>>,
    /// Encrypts what goes to `snapshot_store`. `None`: written in the clear
    /// (the daemon allows that for a local store only).
    pub archive_key: Option<SealKey>,
    /// Reads unencrypted objects (written before encryption). Local stores only.
    pub accept_plaintext: bool,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            image: "ferrobot-runner:dev".into(),
            name_prefix: "ferrobot-".into(),
            owner: "local".into(),
            home_dir: "/home/node".into(),
            memory_bytes: 2 * 1024 * 1024 * 1024,
            pids_limit: 512,
            snapshot_repo: "ferrobot-snapshot".into(),
            snapshot_store: None,
            archive_key: None,
            accept_plaintext: true,
        }
    }
}

/// One hardened container per agent: no capabilities, `no-new-privileges`, memory and pid limits,
/// the image's non-root user, and `$HOME` on a named volume that `stop`/`destroy` keep.
#[derive(Debug, Clone)]
pub struct DockerBackend {
    /// Swapped by [`ping`](Self::ping) when an engine that was missing comes up.
    docker: Arc<RwLock<Docker>>,
    config: DockerConfig,
}

impl DockerBackend {
    /// Connects using `DOCKER_HOST` or the platform default socket. No socket yet (Docker not
    /// installed or not started) is not an error: [`ping`](Self::ping) connects once it exists.
    pub fn connect(config: DockerConfig) -> Result<Self, SandboxError> {
        let docker = match Docker::connect_with_local_defaults() {
            Err(bollard::errors::Error::SocketNotFoundError(socket)) => {
                tracing::warn!(%socket, "no Docker socket yet");
                // Port 9 (discard) never answers, so every call fails until `ping` reconnects.
                Docker::connect_with_http("http://127.0.0.1:9", 5, bollard::API_DEFAULT_VERSION)?
            }
            connected => connected?,
        };
        Ok(Self {
            docker: Arc::new(RwLock::new(docker)),
            config,
        })
    }

    fn docker(&self) -> Docker {
        self.docker
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Fails fast if the engine is unreachable; reconnects when it has come up since.
    pub async fn ping(&self) -> Result<(), SandboxError> {
        if self.docker().ping().await.is_ok() {
            return Ok(());
        }
        let fresh = Docker::connect_with_local_defaults()?;
        fresh.ping().await?;
        *self.docker.write().unwrap_or_else(PoisonError::into_inner) = fresh;
        Ok(())
    }

    async fn has_image(&self, image: &str) -> Result<bool, SandboxError> {
        Ok(unless_status(self.docker().inspect_image(image).await, &[404])?.is_some())
    }

    async fn inspect(&self, id: &str) -> Result<ContainerInspectResponse, SandboxError> {
        self.docker()
            .inspect_container(id, None::<InspectContainerOptions>)
            .await
            .map_err(|e| not_found_or(e, id))
    }

    /// Builds `tag` from a single-file context; a failed `apt-get` reports its own `E:` line.
    async fn build(&self, tag: &str, dockerfile: &str) -> Result<(), SandboxError> {
        let context = single_file_tar("Dockerfile", dockerfile.as_bytes())?;
        let options = BuildImageOptionsBuilder::default()
            .dockerfile("Dockerfile")
            .t(tag)
            .rm(true)
            .forcerm(true)
            .build();
        let docker = self.docker();
        let mut steps = docker.build_image(options, None, Some(bollard::body_full(context.into())));
        let mut apt_error = None;
        while let Some(step) = steps.next().await {
            match step {
                Ok(info) => {
                    let lines = info.stream.unwrap_or_default();
                    // The legacy builder colors stderr: "\x1b[91mE: Unable to locate ...".
                    let plain = lines.lines().map(|l| l.trim_start_matches("\u{1b}[91m"));
                    if let Some(line) = plain.rev().find(|l| l.starts_with("E: ")) {
                        apt_error = Some(line.to_owned());
                    }
                }
                Err(bollard::errors::Error::DockerStreamError { error }) => {
                    return Err(SandboxError::Other(apt_error.unwrap_or(error)));
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Removes images from `build_image` other than `keep`; ones a sandbox still uses stay.
    async fn sweep_cli_images(&self, keep: Option<&str>) -> Result<(), SandboxError> {
        let own = format!("{CLI_TAG_PREFIX}{}-", self.config.owner);
        let filters = HashMap::from([("reference", vec![format!("{own}*")])]);
        let options = ListImagesOptionsBuilder::default()
            .filters(&filters)
            .build();
        let images = self.docker().list_images(Some(options)).await?;
        let stale = images
            .iter()
            .flat_map(|i| &i.repo_tags)
            .filter(|t| t.starts_with(&own) && Some(t.as_str()) != keep);
        for tag in stale {
            // 409: a container still uses it; a later sweep removes it.
            let removal = self
                .docker()
                .remove_image(tag, None::<RemoveImageOptions>, None)
                .await;
            unless_status(removal, &[404, 409])?;
        }
        Ok(())
    }

    /// Downloads the runner image as soon as the engine answers, so the first wake after an
    /// install or update does not wait for it. Gives up after one failed pull.
    pub async fn prefetch_image(&self) {
        while self.ping().await.is_err() {
            tokio::time::sleep(PREFETCH_RETRY).await;
        }
        if let Err(e) = self.pull_if_missing(&self.config.image).await {
            tracing::warn!(error = %e, "could not download the runner image ahead of the first wake");
        }
    }

    /// Pulls `image` unless the engine has it (a release build names a registry image).
    async fn pull_if_missing(&self, image: &str) -> Result<(), SandboxError> {
        if self.has_image(image).await? {
            return Ok(());
        }
        tracing::info!(%image, "pulling the runner image; the first start takes a while");
        let options = CreateImageOptionsBuilder::default()
            .from_image(image)
            .build();
        let mut pull = self.docker().create_image(Some(options), None, None);
        while let Some(step) = pull.next().await {
            if let Some(error) = step?.error_detail.and_then(|d| d.message) {
                return Err(SandboxError::Other(format!("pulling {image}: {error}")));
            }
        }
        Ok(())
    }

    fn container_name(&self, name: &str) -> String {
        format!("{}{name}", self.config.name_prefix)
    }

    fn volume_name(&self, name: &str) -> String {
        format!("{}{name}-home", self.config.name_prefix)
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

    /// Unpacks a home-directory tar (as `snapshot` stores it) into the container.
    async fn upload_home(&self, id: &str, archive: Bytes) -> Result<(), SandboxError> {
        let parent = std::path::Path::new(&self.config.home_dir)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("/");
        let options = UploadToContainerOptionsBuilder::default()
            .path(parent)
            .copy_uidgid("true")
            .build();
        self.docker()
            .upload_to_container(id, Some(options), bollard::body_full(archive))
            .await?;
        Ok(())
    }

    fn sealer(&self, key: &ObjectPath) -> Result<Option<Sealer>, SandboxError> {
        self.config
            .archive_key
            .as_ref()
            .map(|k| Sealer::new(k, key.as_ref()))
            .transpose()
    }

    fn opener(&self, key: &ObjectPath) -> Opener {
        Opener::new(
            self.config.archive_key.as_ref(),
            key.as_ref(),
            self.config.accept_plaintext,
        )
    }

    /// Reads a whole object from the store and decrypts it.
    async fn get_opened(
        &self,
        store: &dyn ObjectStore,
        key: &ObjectPath,
    ) -> Result<Bytes, SandboxError> {
        let bytes = store.get(key).await?.bytes().await?;
        let config = &self.config;
        let plain = seal::open(
            config.archive_key.as_ref(),
            key.as_ref(),
            &bytes,
            config.accept_plaintext,
        )?;
        Ok(plain.into())
    }

    fn archive_keys(name: &str) -> (ObjectPath, ObjectPath) {
        (
            ObjectPath::from(format!("archives/{name}/image.tar")),
            ObjectPath::from(format!("archives/{name}/home.tar")),
        )
    }

    /// Moves a sandbox into the store: its committed image and home directory
    /// go to `archives/<name>/`; container, volume and image are removed.
    async fn archive_sandbox(&self, sandbox: &SandboxHandle) -> Result<(), SandboxError> {
        let id = sandbox.id_for(KIND)?;
        let store = self
            .config
            .snapshot_store
            .as_deref()
            .ok_or(SandboxError::NotImplemented {
                backend: KIND,
                operation: "archive (no object store configured)",
            })?;
        let info = self.inspect(id).await?;
        let full = info.name.unwrap_or_default();
        let name = full
            .trim_start_matches('/')
            .strip_prefix(&self.config.name_prefix)
            .ok_or_else(|| SandboxError::Other(format!("{full} is not a ferrobot sandbox")))?
            .to_owned();
        let (image_key, home_key) = Self::archive_keys(&name);
        let home = DownloadFromContainerOptionsBuilder::default()
            .path(&self.config.home_dir)
            .build();
        put_stream(
            store,
            &home_key,
            self.docker().download_from_container(id, Some(home)),
            self.sealer(&home_key)?,
        )
        .await?;
        let blank = blank_token_env(
            info.config
                .and_then(|c| c.env)
                .as_deref()
                .unwrap_or_default(),
        );
        let options = CommitContainerOptionsBuilder::default()
            .container(id)
            .repo(ARCHIVE_REPO)
            .tag(&name)
            .comment("ferrobot archive")
            .changes(&blank)
            .build();
        self.docker()
            .commit_container(options, bollard::models::ContainerConfig::default())
            .await?;
        let image = format!("{ARCHIVE_REPO}:{name}");
        // shortcut: saves the whole image (base layers too, ~1 GiB); upgrade to
        // shipping only the container's diff once archives pile up.
        put_stream(
            store,
            &image_key,
            self.docker().export_image(&image),
            self.sealer(&image_key)?,
        )
        .await?;
        self.destroy(sandbox, true).await?;
        let force = RemoveImageOptionsBuilder::default().force(true).build();
        self.docker()
            .remove_image(&image, Some(force), None)
            .await?;
        Ok(())
    }

    /// Brings an archived sandbox back (image, then home), if `spec.name` has one.
    async fn unarchive(&self, spec: &SandboxSpec) -> Result<Option<String>, SandboxError> {
        let Some(store) = self.config.snapshot_store.as_deref() else {
            return Ok(None);
        };
        let (image_key, home_key) = Self::archive_keys(&spec.name);
        let image_tar = match store.get(&image_key).await {
            Ok(found) => opened_stream(self.opener(&image_key), found.into_stream()),
            Err(object_store::Error::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut load = self.docker().import_image_stream(
            ImportImageOptionsBuilder::default().quiet(true).build(),
            image_tar,
            None,
        );
        while let Some(step) = load.next().await {
            if let Some(error) = step?.error_detail.and_then(|d| d.message) {
                return Err(SandboxError::Other(format!(
                    "loading archived image: {error}"
                )));
            }
        }
        let id = self
            .create_container(spec, &format!("{ARCHIVE_REPO}:{}", spec.name))
            .await?;
        self.upload_home(&id, self.get_opened(store, &home_key).await?)
            .await?;
        self.ensure_started(&id).await?;
        self.delete_archive(&spec.name).await?;
        Ok(Some(id))
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
            cmd: spec.command.clone(),
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
        match self.docker().create_container(Some(options), body).await {
            Ok(created) => Ok(created.id),
            // Adopt an existing container with the same name (idempotent create).
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 409, ..
            }) => {
                tracing::info!(container = %name, "adopting existing container");
                self.docker()
                    .inspect_container(&name, None::<InspectContainerOptions>)
                    .await?
                    .id
                    .ok_or_else(|| SandboxError::Other("container without id".into()))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn ensure_started(&self, id: &str) -> Result<(), SandboxError> {
        let info = self.inspect(id).await?;
        let state = info.state.unwrap_or_default();
        if state.paused == Some(true) {
            self.docker().unpause_container(id).await?;
        } else if state.running != Some(true) {
            self.docker().start_container(id, None).await?;
        }
        Ok(())
    }
}

/// Streams `chunks` into the store as one object (multipart, aborted on
/// error), encrypted when a sealer is given.
async fn put_stream<S>(
    store: &dyn ObjectStore,
    key: &ObjectPath,
    mut chunks: S,
    mut sealer: Option<Sealer>,
) -> Result<(), SandboxError>
where
    S: futures_util::Stream<Item = Result<Bytes, bollard::errors::Error>> + Unpin,
{
    let mut upload = object_store::WriteMultipart::new(store.put_multipart(key).await?);
    while let Some(chunk) = chunks.next().await {
        let out = match (chunk, sealer.as_mut()) {
            (Ok(chunk), Some(s)) => s.push(&chunk),
            (Ok(chunk), None) => Ok(chunk.to_vec()),
            (Err(e), _) => Err(e.into()),
        };
        match out {
            Ok(bytes) => upload.write(&bytes),
            Err(e) => {
                upload.abort().await?;
                return Err(e);
            }
        }
    }
    if let Some(s) = sealer {
        upload.write(&s.finish()?);
    }
    upload.finish().await?;
    Ok(())
}

/// Decrypts an object as it streams from the store.
fn opened_stream(
    opener: Opener,
    chunks: futures_util::stream::BoxStream<'static, object_store::Result<Bytes>>,
) -> impl futures_util::Stream<Item = Result<Bytes, SandboxError>> + Send + 'static {
    futures_util::stream::try_unfold(
        (Some(opener), chunks),
        |(mut opener, mut chunks)| async move {
            loop {
                let Some(o) = opener.as_mut() else {
                    return Ok(None);
                };
                let plain = match chunks.next().await {
                    Some(chunk) => o.push(&chunk?)?,
                    None => opener.take().map_or(Ok(Vec::new()), Opener::finish)?,
                };
                if !plain.is_empty() || opener.is_none() {
                    return Ok(Some((Bytes::from(plain), (opener, chunks))));
                }
            }
        },
    )
}

/// `None` when the engine answered with one of `statuses`.
fn unless_status<T>(
    result: Result<T, bollard::errors::Error>,
    statuses: &[u16],
) -> Result<Option<T>, SandboxError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(bollard::errors::Error::DockerResponseServerError { status_code, .. })
            if statuses.contains(&status_code) =>
        {
            Ok(None)
        }
        Err(e) => Err(e.into()),
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

    fn home(&self) -> String {
        self.config.home_dir.clone()
    }

    fn supports_image_builds(&self) -> bool {
        true
    }

    async fn build_image(&self, packages: &[String]) -> Result<Option<String>, SandboxError> {
        let packages = crate::package_set(packages)?;
        let image = if packages.is_empty() {
            None
        } else {
            self.pull_if_missing(&self.config.image).await?;
            let base = self.docker().inspect_image(&self.config.image).await?;
            let base_id = base.id.unwrap_or_default();
            let user = base
                .config
                .and_then(|c| c.user)
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| "root".into());
            let tag = cli_tag(&self.config.owner, &base_id, &packages);
            if !self.has_image(&tag).await? {
                tracing::info!(%tag, ?packages, "building the CLI image");
                self.build(&tag, &dockerfile(&base_id, &user, &packages)?)
                    .await?;
            }
            Some(tag)
        };
        self.sweep_cli_images(image.as_deref()).await?;
        Ok(image)
    }

    async fn is_stale(
        &self,
        sandbox: &SandboxHandle,
        image: Option<&str>,
    ) -> Result<bool, SandboxError> {
        let id = sandbox.id_for(KIND)?;
        let inspected = self
            .docker()
            .inspect_container(id, None::<InspectContainerOptions>)
            .await;
        let Some(info) = unless_status(inspected, &[404])? else {
            return Ok(false);
        };
        let current = info.config.and_then(|c| c.image).unwrap_or_default();
        let runs = info.image.unwrap_or_default();
        let wanted = image.unwrap_or(&self.config.image);
        let now = if current == wanted {
            unless_status(self.docker().inspect_image(wanted).await, &[404])?.and_then(|i| i.id)
        } else {
            None
        };
        let ids = now.as_deref().map(|now| (runs.as_str(), now));
        Ok(stale(&current, image, &self.config.image, ids))
    }

    async fn ready(&self) -> Result<(), SandboxError> {
        self.ping().await
    }

    async fn default_image(&self) -> Option<(String, Option<String>)> {
        let image = &self.config.image;
        let id = self
            .docker()
            .inspect_image(image)
            .await
            .ok()
            .and_then(|i| i.id);
        Some((image.clone(), id))
    }

    async fn create(&self, spec: &SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        let id = if let Some(id) = self.unarchive(spec).await? {
            id
        } else {
            let image = spec.image.as_deref().unwrap_or(&self.config.image);
            self.pull_if_missing(image).await?;
            let id = self.create_container(spec, image).await?;
            self.ensure_started(&id).await?;
            id
        };
        Ok(SandboxHandle {
            backend: KIND.into(),
            id,
        })
    }

    async fn exec_stream(
        &self,
        sandbox: &SandboxHandle,
        cmd: Vec<String>,
    ) -> Result<crate::ExecStream, SandboxError> {
        let id = sandbox.id_for(KIND)?;
        let exec = self
            .docker()
            .create_exec(
                id,
                ExecConfig {
                    cmd: Some(cmd),
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(false),
                    ..ExecConfig::default()
                },
            )
            .await
            .map_err(|e| not_found_or(e, id))?;
        match self
            .docker()
            .start_exec(&exec.id, None::<StartExecOptions>)
            .await?
        {
            StartExecResults::Attached { output, input } => Ok(crate::ExecStream {
                output: output
                    .filter_map(|chunk| async move {
                        match chunk {
                            Ok(bollard::container::LogOutput::StdErr { .. }) => None,
                            Ok(out) => Some(Ok(out.into_bytes())),
                            Err(e) => Some(Err(e.into())),
                        }
                    })
                    .boxed(),
                input,
            }),
            StartExecResults::Detached => {
                Err(SandboxError::Other("the exec did not attach".into()))
            }
        }
    }

    async fn exec(
        &self,
        sandbox: &SandboxHandle,
        req: &ExecRequest,
    ) -> Result<ExecOutput, SandboxError> {
        let id = sandbox.id_for(KIND)?;
        let exec = self
            .docker()
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
            .docker()
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
            .docker()
            .inspect_exec(&exec.id)
            .await?
            .exit_code
            .unwrap_or(-1);
        Ok(ExecOutput::captured(exit_code, &stdout, &stderr))
    }

    async fn read_file(
        &self,
        sandbox: &SandboxHandle,
        path: &SandboxPath,
    ) -> Result<Vec<u8>, SandboxError> {
        let id = sandbox.id_for(KIND)?;
        let options = DownloadFromContainerOptionsBuilder::default()
            .path(path.as_str())
            .build();
        let mut stream = self.docker().download_from_container(id, Some(options));
        let mut tar = Vec::new();
        while let Some(chunk) = stream
            .try_next()
            .await
            .map_err(|e| not_found_or(e, path.as_str()))?
        {
            // The tar adds headers; stop well before a huge file fills memory.
            if tar.len() + chunk.len() > MAX_FILE_BYTES + 64 * 1024 {
                return Err(SandboxError::TooLarge(path.to_string()));
            }
            tar.extend_from_slice(&chunk);
        }
        first_file_from_tar(&tar, path.as_str())
    }

    async fn write_file(
        &self,
        sandbox: &SandboxHandle,
        path: &SandboxPath,
        contents: &[u8],
    ) -> Result<(), SandboxError> {
        let id = sandbox.id_for(KIND)?;
        let archive = single_file_tar(path.file_name(), contents)?;
        let options = UploadToContainerOptionsBuilder::default()
            .path(path.parent())
            .build();
        self.docker()
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
        let id = sandbox.id_for(KIND)?;
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
                .docker()
                .download_from_container(id, Some(options))
                .try_collect()
                .await
                .map_err(|e| not_found_or(e, id))?;
            // shortcut: the whole home archive is buffered and sent as one PUT (S3 caps
            // that at 5 GiB); switch to `put_multipart` once homes get that large.
            let mut tar = chunks.concat();
            if let Some(k) = &self.config.archive_key {
                tar = seal::seal(k, key.as_ref(), &tar)?;
            }
            store.put(key, tar.into()).await?;
        }
        let env = self
            .inspect(id)
            .await?
            .config
            .and_then(|c| c.env)
            .unwrap_or_default();
        let blank = blank_token_env(&env);
        let options = CommitContainerOptionsBuilder::default()
            .container(id)
            .repo(&self.config.snapshot_repo)
            .tag(&tag)
            .comment("ferrobot snapshot")
            .changes(&blank)
            .build();
        self.docker()
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
        sandbox.id_for(KIND)?;
        self.docker()
            .inspect_image(image)
            .await
            .map_err(|e| not_found_or(e, image))?;
        let archive = match self.archive(image) {
            Some((store, key)) => match self.get_opened(store, &key).await {
                Ok(found) => Some(found),
                Err(SandboxError::Storage(object_store::Error::NotFound { .. })) => None,
                Err(e) => return Err(e),
            },
            None => None,
        };
        self.destroy(sandbox, archive.is_some()).await?;
        let id = self.create_container(spec, image).await?;
        if let Some(archive) = archive {
            self.upload_home(&id, archive).await?;
        }
        self.ensure_started(&id).await?;
        Ok(SandboxHandle {
            backend: KIND.into(),
            id,
        })
    }

    async fn delete_archive(&self, name: &str) -> Result<(), SandboxError> {
        let image = format!("{ARCHIVE_REPO}:{name}");
        // 409: a restored container still uses the image; it goes with a later purge.
        let removal = self
            .docker()
            .remove_image(&image, None::<RemoveImageOptions>, None)
            .await;
        unless_status(removal, &[404, 409])?;
        let Some(store) = self.config.snapshot_store.as_deref() else {
            return Ok(());
        };
        let (image_key, home_key) = Self::archive_keys(name);
        for key in [image_key, home_key] {
            match store.delete(&key).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    async fn delete_snapshot(&self, snapshot: &str) -> Result<(), SandboxError> {
        let image = self.check_snapshot(snapshot)?;
        let options = RemoveImageOptionsBuilder::default().force(true).build();
        unless_status(
            self.docker().remove_image(image, Some(options), None).await,
            &[404],
        )?;
        if let Some((store, key)) = self.archive(image) {
            match store.delete(&key).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    async fn wake(&self, sandbox: &SandboxHandle) -> Result<(), SandboxError> {
        self.ensure_started(sandbox.id_for(KIND)?).await
    }

    async fn sleep(
        &self,
        sandbox: &SandboxHandle,
        level: SleepLevel,
    ) -> Result<SleepLevel, SandboxError> {
        match level {
            SleepLevel::Cold => {
                self.stop(sandbox).await?;
                return Ok(level);
            }
            SleepLevel::Archived => {
                self.archive_sandbox(sandbox).await?;
                return Ok(level);
            }
            SleepLevel::Warm => {}
        }
        let id = sandbox.id_for(KIND)?;
        match self.docker().pause_container(id).await {
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
        let id = sandbox.id_for(KIND)?;
        // A paused container cannot handle SIGTERM; resume it so it stops cleanly.
        if self.state(sandbox).await? == SandboxState::Paused {
            self.docker().unpause_container(id).await?;
        }
        let options = StopContainerOptionsBuilder::default().t(10).build();
        match self.docker().stop_container(id, Some(options)).await {
            // 304: already stopped
            Ok(())
            | Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
            Err(e) => Err(not_found_or(e, id)),
        }
    }

    async fn destroy(&self, sandbox: &SandboxHandle, purge_disk: bool) -> Result<(), SandboxError> {
        let id = sandbox.id_for(KIND)?;
        // Resolve the volume before the container disappears.
        let volume = if purge_disk {
            self.docker()
                .inspect_container(id, None::<InspectContainerOptions>)
                .await
                .ok()
                .and_then(|info| info.mounts)
                .and_then(|mounts| mounts.into_iter().find_map(|m| m.name))
        } else {
            None
        };
        let options = RemoveContainerOptionsBuilder::default().force(true).build();
        unless_status(
            self.docker().remove_container(id, Some(options)).await,
            &[404],
        )?;
        if let Some(volume) = volume {
            self.docker()
                .remove_volume(&volume, None::<RemoveVolumeOptions>)
                .await?;
        }
        Ok(())
    }

    async fn state(&self, sandbox: &SandboxHandle) -> Result<SandboxState, SandboxError> {
        let id = sandbox.id_for(KIND)?;
        let inspected = self
            .docker()
            .inspect_container(id, None::<InspectContainerOptions>)
            .await;
        let Some(info) = unless_status(inspected, &[404])? else {
            return Ok(SandboxState::Missing);
        };
        let state = info.state.unwrap_or_default();
        Ok(if state.paused == Some(true) {
            SandboxState::Paused
        } else if state.running == Some(true) {
            SandboxState::Running
        } else {
            SandboxState::Stopped
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_variable_holding_the_token_is_blanked() {
        let env = [
            "FERROBOT_AGENT_TOKEN=tok",
            "ANTHROPIC_API_KEY=tok",
            "GEMINI_API_KEY=tok",
            "AWS_BEARER_TOKEN_BEDROCK=tok",
            "HTTPS_PROXY=http://ferrobot:tok@host:7879",
            "FERROBOT_DRIVER=mock",
            "PATH=/usr/bin",
        ]
        .map(String::from);
        assert_eq!(
            super::blank_token_env(&env),
            "ENV FERROBOT_AGENT_TOKEN= ANTHROPIC_API_KEY= GEMINI_API_KEY= AWS_BEARER_TOKEN_BEDROCK= \
             HTTPS_PROXY="
        );
        assert_eq!(
            super::blank_token_env(&["FERROBOT_AGENT_TOKEN=".into(), "X=".into()]),
            "ENV FERROBOT_AGENT_TOKEN="
        );
    }

    use super::*;

    #[test]
    fn cli_image_passes_packages_as_arguments() {
        let packages = ["jq".to_owned(), "g++".to_owned()];
        let file = dockerfile("sha256:ab12", "node", &packages).unwrap();
        let lines: Vec<&str> = file.lines().collect();
        assert_eq!(lines[..2], ["FROM sha256:ab12", "USER root"]);
        assert_eq!(lines[3], "USER node");
        let argv: Vec<String> =
            serde_json::from_str(lines[2].strip_prefix("RUN ").unwrap()).unwrap();
        assert_eq!(argv[..4], ["sh", "-c", INSTALL, "sh"]);
        assert!(INSTALL.contains("/etc/chromium.d/ferrobot") && INSTALL.contains("--no-sandbox"));
        assert_eq!(argv[4..], packages);
        assert!(dockerfile("sha256:ab12", "node\nRUN id", &packages).is_err());
        assert!(dockerfile("x y", "node", &packages).is_err());

        let tag = cli_tag("local", "sha256:ab12", &packages);
        assert!(
            tag.starts_with("ferrobot-runner:clis-local-")
                && tag.len() == CLI_TAG_PREFIX.len() + 22
        );
        assert_eq!(tag, cli_tag("local", "sha256:ab12", &packages));
        assert_ne!(tag, cli_tag("local", "sha256:cd34", &packages));
        assert_ne!(tag, cli_tag("local", "sha256:ab12", &packages[..1]));
        assert_ne!(
            tag,
            cli_tag("other", "sha256:ab12", &packages),
            "per daemon"
        );
    }

    #[test]
    fn only_base_and_cli_containers_go_stale() {
        let base = "ferrobot-runner:dev";
        let (old, new) = ("ferrobot-runner:clis-aa", "ferrobot-runner:clis-bb");
        for (current, wanted, expected) in [
            ("ferrobot-snapshot:x-1", Some(new), false),
            ("ferrobot-archive:ada", Some(new), false),
            ("ferrobot-runner:0.1", Some(new), false),
            (base, Some(new), true),
            (old, Some(new), true),
            (old, None, true),
            (new, Some(new), false),
            (base, None, false),
        ] {
            assert_eq!(
                stale(current, wanted, base, None),
                expected,
                "{current} -> {wanted:?}"
            );
        }
        // The same name over a rebuilt image (a new runner) is stale; snapshots never are.
        let rebuilt = Some(("sha256:old", "sha256:new"));
        assert!(stale(base, None, base, rebuilt));
        assert!(stale(new, Some(new), base, rebuilt));
        assert!(!stale(
            base,
            None,
            base,
            Some(("sha256:same", "sha256:same"))
        ));
        assert!(!stale("ferrobot-snapshot:x-1", None, base, rebuilt));
    }

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
            h.id_for(KIND),
            Err(SandboxError::WrongBackend { .. })
        ));
    }
}
