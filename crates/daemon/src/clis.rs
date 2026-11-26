//! CLIs in the agents' computers. Debian packages go into one shared image (`build_image`), which a
//! sandbox picks up when it is next created or woken; npm packages, downloads and scripts go into
//! the home folder of each CLI's agents, once per agent (a marker under `~/.ferrobot/clis`).

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use ferrobot_core::{AgentId, AgentStatus, Cli, Install};
use ferrobot_sandbox::{ExecRequest, SandboxHandle, SandboxPath};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::error::ApiError;

const SETTING: &str = "clis";
const BUILD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MARKERS: &str = ".ferrobot/clis";
/// The runner image's `PATH`; home installs come first.
pub const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// Fetches `$1` into `~/.local/bin/$3`, checking SHA-256 `$2` when given.
const DOWNLOAD: &str = "set -e; d=\"$HOME/.local/bin\"; mkdir -p \"$d\"; t=\"$(mktemp)\"; \
    curl -fsSL --proto '=https' \"$1\" -o \"$t\"; \
    if [ -n \"$2\" ]; then echo \"$2  $t\" | sha256sum -c - >/dev/null; fi; \
    chmod +x \"$t\"; mv \"$t\" \"$d/$3\"";

/// The operator's CLIs and the image last built from their Debian packages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Stored {
    /// Before 0.1.11 the list held Debian package names only; read once into `clis`.
    #[serde(skip_serializing)]
    packages: Vec<String>,
    pub clis: Vec<Cli>,
    /// Last successful build; `None`: the base image.
    pub image: Option<String>,
}

/// Where the image build stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    /// No CLIs: sandboxes run the base image.
    #[default]
    Idle,
    Building,
    /// New and woken sandboxes start from the image with these CLIs.
    Ready,
    /// The last build failed; sandboxes keep the previous image.
    Failed,
}

/// In memory: the build, home installs still due, their failures and running sign-ins.
#[derive(Debug, Default)]
pub struct Builds {
    build: Mutex<(BuildStatus, Option<String>)>,
    /// Agents whose home installs changed while they slept.
    pending: Mutex<HashSet<AgentId>>,
    /// The last failed home install per CLI.
    errors: Mutex<BTreeMap<String, String>>,
    pub(crate) sign_ins: crate::signin::SignIns,
}

impl Builds {
    pub(crate) fn get(&self) -> (BuildStatus, Option<String>) {
        crate::lock(&self.build).clone()
    }

    fn set(&self, status: BuildStatus, error: Option<String>) {
        *crate::lock(&self.build) = (status, error);
    }

    /// Marks a build as started; `false` if one is running.
    fn start(&self) -> bool {
        let mut current = crate::lock(&self.build);
        if current.0 == BuildStatus::Building {
            return false;
        }
        *current = (BuildStatus::Building, None);
        true
    }

    pub(crate) fn error_of(&self, cli: &str) -> Option<String> {
        crate::lock(&self.errors).get(cli).cloned()
    }

    /// Whether `agent` has home installs to catch up on; clears it.
    pub(crate) fn take_pending(&self, agent: AgentId) -> bool {
        crate::lock(&self.pending).remove(&agent)
    }
}

pub(crate) async fn load(state: &AppState) -> Result<Stored, ApiError> {
    let mut stored = match state.store.get_setting(SETTING).await? {
        Some(raw) => serde_json::from_str::<Stored>(&raw)
            .map_err(|e| ApiError::Internal(format!("CLI settings: {e}")))?,
        None => Stored::default(),
    };
    for package in std::mem::take(&mut stored.packages) {
        if !stored.clis.iter().any(|c| c.id == package) {
            stored.clis.push(debian(package));
        }
    }
    Ok(stored)
}

async fn save(state: &AppState, stored: &Stored) -> Result<(), ApiError> {
    let raw = serde_json::to_string(stored).map_err(|e| ApiError::Internal(e.to_string()))?;
    state.store.put_setting(SETTING, raw).await?;
    Ok(())
}

fn debian(package: String) -> Cli {
    Cli {
        id: package.clone(),
        install: Install::Debian { package },
        programs: Vec::new(),
        account: None,
        agents: Vec::new(),
    }
}

/// Every CLI; none if the settings cannot be read.
pub(crate) async fn all(state: &AppState) -> Vec<Cli> {
    match load(state).await {
        Ok(stored) => stored.clis,
        Err(e) => {
            tracing::error!(error = %e, "cannot read the CLIs");
            Vec::new()
        }
    }
}

/// The Debian packages in the image, sorted.
pub(crate) fn packages(clis: &[Cli]) -> Vec<String> {
    let mut packages: Vec<String> = clis
        .iter()
        .filter_map(|c| match &c.install {
            Install::Debian { package } => Some(package.clone()),
            _ => None,
        })
        .collect();
    packages.sort();
    packages.dedup();
    packages
}

/// The image new sandboxes start from; `None`: the backend's default.
pub(crate) async fn image(state: &AppState) -> Result<Option<String>, ApiError> {
    Ok(load(state).await?.image)
}

/// Keeps exactly `packages` as plain Debian CLIs (the `PUT /v1/clis` of before 0.1.11); other CLIs
/// stay.
pub(crate) async fn apply(state: &AppState, packages: Vec<String>) -> Result<(), ApiError> {
    let mut stored = load(state).await?;
    let before = stored.clis.clone();
    stored.clis.retain(|c| {
        !matches!(c.install, Install::Debian { .. }) || c.account.is_some() || !c.agents.is_empty()
    });
    for package in packages {
        if !stored.clis.iter().any(|c| c.id == package) {
            stored.clis.push(debian(package));
        }
    }
    stored.clis.sort_by(|a, b| a.id.cmp(&b.id));
    commit(state, stored, &before).await
}

/// Adds or replaces `cli`. Agents whose token setup changes restart (files kept); home installs
/// follow at once for running agents and on the next wake for the others.
pub(crate) async fn put(state: &AppState, cli: Cli) -> Result<(), ApiError> {
    let mut stored = load(state).await?;
    cli.validate(&stored.clis).map_err(ApiError::BadRequest)?;
    if let Install::Debian { package } = &cli.install {
        ferrobot_sandbox::package_set(std::slice::from_ref(package))
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    }
    if !state.backend.supports_image_builds() {
        return Err(ApiError::BadRequest("CLIs need the Docker backend".into()));
    }
    let before = stored.clis.clone();
    stored.clis.retain(|c| c.id != cli.id);
    stored.clis.push(cli);
    stored.clis.sort_by(|a, b| a.id.cmp(&b.id));
    commit(state, stored, &before).await
}

/// Removes a CLI, its token and its CA, from the image or its agents' home folders.
pub(crate) async fn remove(state: &AppState, id: &str) -> Result<(), ApiError> {
    let mut stored = load(state).await?;
    let before = stored.clis.clone();
    if !stored.clis.iter().any(|c| c.id == id) {
        return Err(ApiError::NotFound(format!("CLI {id:?} not found")));
    }
    stored.clis.retain(|c| c.id != id);
    commit(state, stored, &before).await?;
    state
        .settings
        .secrets
        .delete(&crate::logins::secret_key(id))
        .await?;
    crate::egress::forget(state, id)
        .await
        .map_err(ApiError::Internal)
}

/// Saves `stored` and carries out what changed since `before`: a build when the Debian packages
/// differ, restarts where a token setup changed, home installs for the rest.
async fn commit(state: &AppState, stored: Stored, before: &[Cli]) -> Result<(), ApiError> {
    let rebuild = packages(before) != packages(&stored.clis);
    if rebuild && !state.clis.start() {
        return Err(ApiError::Conflict(
            "the CLI image is still building; try again when it is done".into(),
        ));
    }
    if let Err(e) = save(state, &stored).await {
        if rebuild {
            state.clis.set(BuildStatus::Failed, Some(e.to_string()));
        }
        return Err(e);
    }
    if rebuild {
        let state = state.clone();
        tokio::spawn(async move { build(&state).await });
    }
    let mut restart = HashSet::new();
    let mut home = HashSet::new();
    let old = |id: &str| before.iter().find(|c| c.id == id);
    let ids: HashSet<&str> = before
        .iter()
        .chain(&stored.clis)
        .map(|c| c.id.as_str())
        .collect();
    for id in ids {
        let (was, now) = (old(id), stored.clis.iter().find(|c| c.id == id));
        if was == now {
            continue;
        }
        let agents = |c: Option<&Cli>| c.map(|c| c.agents.clone()).unwrap_or_default();
        if token_setup(was) != token_setup(now) {
            restart.extend(agents(was).into_iter().chain(agents(now)));
        } else if token_setup(now).is_some() {
            let (a, b): (HashSet<_>, HashSet<_>) = (
                agents(was).into_iter().collect(),
                agents(now).into_iter().collect(),
            );
            restart.extend(a.symmetric_difference(&b).copied());
        }
        let in_home = |c: Option<&Cli>| c.is_some_and(|c| c.install.is_home());
        if in_home(was) || in_home(now) {
            home.extend(agents(was).into_iter().chain(agents(now)));
        }
    }
    for agent in restart.iter().copied() {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::lifecycle::recreate(&state, agent).await {
                tracing::warn!(%agent, error = %e, "computer not restarted for a CLI change");
            }
        });
    }
    // A restarted computer installs on provisioning.
    for agent in home.difference(&restart).copied() {
        let running = state
            .store
            .get_agent(agent)
            .await
            .is_ok_and(|a| a.status == AgentStatus::Running);
        if running {
            let state = state.clone();
            tokio::spawn(async move { sync_home(&state, agent).await });
        } else {
            crate::lock(&state.clis.pending).insert(agent);
        }
    }
    Ok(())
}

/// What a CLI's agents get in their environment for its account; a change restarts them.
fn token_setup(c: Option<&Cli>) -> Option<(&Vec<String>, &Vec<String>)> {
    c.and_then(|c| c.account.as_ref())
        .map(|a| (&a.hosts, &a.env))
}

/// Rebuilds on startup, so a rebuilt base image reaches agents with CLIs (a new tag), and catches up
/// on home installs that changed while the daemon was down.
pub fn resume(state: &AppState) {
    if !state.backend.supports_image_builds() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let stored = match load(&state).await {
            Ok(stored) => stored,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the CLI settings");
                return;
            }
        };
        crate::lock(&state.clis.pending).extend(
            stored
                .clis
                .iter()
                .filter(|c| c.install.is_home())
                .flat_map(|c| c.agents.iter().copied()),
        );
        crate::signin::sweep(&state, &stored.clis).await;
        if !packages(&stored.clis).is_empty() && state.clis.start() {
            build(&state).await;
        }
    });
}

async fn build(state: &AppState) {
    let outcome = async {
        let packages = packages(&load(state).await?.clis);
        let image = tokio::time::timeout(BUILD_TIMEOUT, state.backend.build_image(&packages))
            .await
            .map_err(|_| ApiError::Upstream("the build took longer than 15 minutes".into()))??;
        // Reloaded: other CLIs may have changed during the build.
        let mut stored = load(state).await?;
        stored.image = image;
        save(state, &stored).await?;
        Ok::<_, ApiError>(packages)
    }
    .await;
    match outcome {
        Ok(packages) if packages.is_empty() => state.clis.set(BuildStatus::Idle, None),
        Ok(_) => state.clis.set(BuildStatus::Ready, None),
        Err(e) => {
            tracing::warn!(error = %e, "CLI image build failed");
            let message = match e {
                ApiError::Sandbox(e) => e.to_string(),
                other => other.to_string(),
            };
            state.clis.set(BuildStatus::Failed, Some(message));
        }
    }
}

/// Whether `agent` has CLIs in its home folder.
pub(crate) async fn has_home(state: &AppState, agent: AgentId) -> bool {
    all(state)
        .await
        .iter()
        .any(|c| c.install.is_home() && c.agents.contains(&agent))
}

/// The command that installs `cli` into the home folder `home`; also used by the sign-in.
pub(crate) fn install_command(cli: &Cli, home: &str) -> Option<ExecRequest> {
    let prefix = format!("{home}/.local");
    let mut req = match &cli.install {
        Install::Debian { .. } => return None,
        Install::Npm { package } => {
            ExecRequest::new(["npm", "install", "--global", "--prefix", &prefix, package])
        }
        Install::Download { url, sha256 } => {
            let program = cli.programs.first()?;
            ExecRequest::new([
                "sh",
                "-c",
                DOWNLOAD,
                "sh",
                url,
                sha256.as_deref().unwrap_or_default(),
                program,
            ])
        }
        Install::Script { script } => {
            let mut req = ExecRequest::new(["bash", "-c", script]);
            req.env.insert("PREFIX".into(), prefix.clone());
            req
        }
    };
    req.workdir = Some(home.to_owned());
    Some(req)
}

/// What removes `cli` from a home folder; a script's files stay.
fn uninstall_command(cli: &Cli, home: &str) -> Option<ExecRequest> {
    match &cli.install {
        Install::Npm { package } => Some(ExecRequest::new([
            "npm",
            "uninstall",
            "--global",
            "--prefix",
            &format!("{home}/.local"),
            package,
        ])),
        Install::Download { .. } => {
            let program = cli.programs.first()?;
            Some(ExecRequest::new([
                "rm",
                "-f",
                &format!("{home}/.local/bin/{program}"),
            ]))
        }
        Install::Debian { .. } | Install::Script { .. } => None,
    }
}

async fn run(
    state: &AppState,
    handle: &SandboxHandle,
    req: &ExecRequest,
) -> Result<ferrobot_sandbox::ExecOutput, String> {
    let out = tokio::time::timeout(INSTALL_TIMEOUT, state.backend.exec(handle, req))
        .await
        .map_err(|_| "took longer than 10 minutes".to_owned())?
        .map_err(|e| e.to_string())?;
    if out.exit_code == 0 {
        Ok(out)
    } else {
        let tail: String = out.stderr.trim().chars().rev().take(300).collect();
        Err(tail.chars().rev().collect())
    }
}

/// Brings `agent`'s home folder in line with its CLIs: installs what is new or changed (the marker
/// holds the install it ran), removes what it no longer has.
pub(crate) async fn sync_home(state: &AppState, agent: AgentId) {
    let Ok(record) = state.store.get_agent(agent).await else {
        return;
    };
    let Some(handle) = crate::lifecycle::handle_of(&record) else {
        return;
    };
    let home = state.backend.home();
    let list = ExecRequest::new([
        "sh",
        "-c",
        "cd \"$HOME/.ferrobot/clis\" 2>/dev/null || exit 0; \
         for f in *; do [ -f \"$f\" ] && printf '%s\\t%s\\036' \"$f\" \"$(cat \"$f\")\"; done; true",
    ]);
    let Ok(listed) = run(state, &handle, &list).await else {
        tracing::warn!(agent = %record.name, "cannot read the installed CLIs");
        return;
    };
    let installed: BTreeMap<String, Cli> = listed
        .stdout
        .split('\u{1e}')
        .filter_map(|entry| {
            let (id, cli) = entry.split_once('\t')?;
            Some((id.to_owned(), serde_json::from_str(cli).ok()?))
        })
        .collect();
    let wanted: Vec<Cli> = all(state)
        .await
        .into_iter()
        .filter(|c| c.install.is_home() && c.agents.contains(&agent))
        .map(|c| Cli {
            account: None,
            agents: Vec::new(),
            ..c
        })
        .collect();
    let marker = |id: &str| SandboxPath::parse(&format!("{home}/{MARKERS}/{id}"));
    for (id, old) in &installed {
        if wanted.iter().any(|c| &c.id == id) {
            continue;
        }
        if let Some(req) = uninstall_command(old, &home) {
            let _ = run(state, &handle, &req).await;
        }
        let rm = ExecRequest::new(["rm", "-f", &format!("{home}/{MARKERS}/{id}")]);
        let _ = run(state, &handle, &rm).await;
    }
    let mkdir = ExecRequest::new(["mkdir", "-p", &format!("{home}/{MARKERS}")]);
    let _ = run(state, &handle, &mkdir).await;
    for cli in wanted {
        if installed.get(&cli.id) == Some(&cli) {
            continue;
        }
        let Some(req) = install_command(&cli, &home) else {
            continue;
        };
        let outcome = match run(state, &handle, &req).await {
            Ok(_) => {
                let json = serde_json::to_vec(&cli).unwrap_or_default();
                match marker(&cli.id) {
                    Ok(path) => state
                        .backend
                        .write_file(&handle, &path, &json)
                        .await
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                }
            }
            Err(e) => Err(e),
        };
        let mut errors = crate::lock(&state.clis.errors);
        match outcome {
            Ok(()) => {
                errors.remove(&cli.id);
            }
            Err(e) => {
                tracing::warn!(agent = %record.name, cli = %cli.id, error = %e, "CLI install failed");
                errors.insert(cli.id.clone(), format!("{}: {e}", record.name));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_installs_take_their_arguments_without_a_shell_in_between() {
        let cli = |install| Cli {
            id: "example".into(),
            install,
            programs: vec!["example".into()],
            account: None,
            agents: Vec::new(),
        };
        let npm = install_command(
            &cli(Install::Npm {
                package: "@scope/example".into(),
            }),
            "/home/node",
        )
        .unwrap();
        assert_eq!(
            npm.cmd,
            [
                "npm",
                "install",
                "--global",
                "--prefix",
                "/home/node/.local",
                "@scope/example"
            ]
        );
        let get = install_command(
            &cli(Install::Download {
                url: "https://example.com/x".into(),
                sha256: None,
            }),
            "/home/node",
        )
        .unwrap();
        assert_eq!(get.cmd[4..], ["https://example.com/x", "", "example"]);
        let debian = Install::Debian {
            package: "jq".into(),
        };
        assert!(install_command(&cli(debian), "/home/node").is_none());
    }
}
