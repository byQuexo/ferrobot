//! Agent sandbox lifecycle: provision, wake, sleep, restore, destroy, reconcile.
//! `running` means the agent should be awake (a crashed sandbox is restarted).

use std::collections::BTreeMap;

use ferrobot_core::{Agent, AgentId, AgentStatus, ProviderKind};
use ferrobot_sandbox::{SandboxError, SandboxHandle, SandboxSpec, SandboxState, SleepLevel};
use ferrobot_store::SnapshotRecord;
use serde_json::json;

use crate::error::ApiError;
use crate::{AppState, UiEvent};

/// Random bearer token: two v4 UUIDs, 244 random bits in 64 hex chars.
pub(crate) fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

pub fn handle_of(agent: &Agent) -> Option<SandboxHandle> {
    Some(SandboxHandle {
        backend: agent.backend.clone()?,
        id: agent.sandbox_id.clone()?,
    })
}

pub fn sandbox_handle(agent: &Agent) -> Result<SandboxHandle, ApiError> {
    handle_of(agent)
        .ok_or_else(|| ApiError::Conflict(format!("agent {} has no sandbox", agent.name)))
}

/// Issues a fresh runner token and builds the sandbox spec around it.
async fn fresh_spec(state: &AppState, agent: &Agent) -> Result<SandboxSpec, ApiError> {
    let token = new_token();
    state.store.set_agent_token(agent.id, &token).await?;
    let s = &state.settings;
    let provider = match agent.provider_id {
        Some(id) => Some(state.store.get_provider(id).await?),
        None => None,
    };
    let driver = match &provider {
        _ if s.driver == "mock" => "mock",
        Some(p) => p.kind.driver(),
        None => "claude",
    };
    let mut env = BTreeMap::from([
        (
            "FERROBOT_DAEMON_URL".to_owned(),
            s.sandbox_daemon_url.clone(),
        ),
        ("FERROBOT_AGENT_NAME".to_owned(), agent.name.to_string()),
        ("FERROBOT_DRIVER".to_owned(), driver.to_owned()),
    ]);
    // Real credentials never enter the sandbox: the runner talks to the
    // daemon's model proxy and authenticates with the agent token.
    let proxy = format!("{}/model", s.sandbox_daemon_url.trim_end_matches('/'));
    let mut set = |k: &str, v: &str| env.insert(k.to_owned(), v.to_owned());
    match driver {
        "claude" => {
            set("ANTHROPIC_BASE_URL", &proxy);
            set("ANTHROPIC_API_KEY", &token);
            set("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
            let region = provider.as_ref().and_then(|p| p.region.as_deref());
            match provider.as_ref().map(|p| p.kind) {
                Some(ProviderKind::Bedrock) => {
                    set("CLAUDE_CODE_USE_BEDROCK", "1");
                    set("ANTHROPIC_BEDROCK_BASE_URL", &proxy);
                    // The agent token, sent as a bearer; the proxy swaps in the Bedrock key.
                    set("AWS_BEARER_TOKEN_BEDROCK", &token);
                    set("AWS_REGION", region.unwrap_or_default());
                }
                Some(ProviderKind::Vertex) => {
                    set("CLAUDE_CODE_USE_VERTEX", "1");
                    set("ANTHROPIC_VERTEX_BASE_URL", &proxy);
                    // The SDK then sends ANTHROPIC_API_KEY (the agent token) as x-api-key.
                    set("CLAUDE_CODE_SKIP_VERTEX_AUTH", "1");
                    set("CLOUD_ML_REGION", region.unwrap_or_default());
                    let project = provider.as_ref().and_then(|p| p.project.as_deref());
                    set("ANTHROPIC_VERTEX_PROJECT_ID", project.unwrap_or_default());
                }
                _ => {}
            }
        }
        "openai" => {
            set("FERROBOT_OPENAI_BASE_URL", &proxy);
        }
        "codex" => {
            set("FERROBOT_CODEX_BASE_URL", &proxy);
        }
        "acp" => {
            set("FERROBOT_ACP_AGENT", "gemini");
            // The agent token, sent as `x-goog-api-key`; the proxy swaps in the real key.
            set("GEMINI_API_KEY", &token);
            set("GOOGLE_GEMINI_BASE_URL", &proxy);
        }
        _ => {}
    }
    let model = agent
        .model
        .clone()
        .or_else(|| provider.as_ref().and_then(|p| p.default_model.clone()))
        .or_else(|| s.model.clone());
    if let (false, Some(model)) = (driver == "mock", model) {
        set("FERROBOT_MODEL", &model);
    }
    let logins = crate::logins::of_agent(state, agent.id).await;
    let home = state.backend.home();
    if state.backend.supports_image_builds() {
        // Last, so a program an agent drops in its home never stands in for the runner's `node`
        // or the daemon's own tools; home CLIs still run by name.
        env.insert(
            "PATH".to_owned(),
            format!("{}:{home}/.local/bin", crate::clis::DEFAULT_PATH),
        );
    }
    // A login never replaces what the sandbox already set.
    for (k, v) in crate::logins::env(&logins, &s.sandbox_daemon_url, &token, &home) {
        env.entry(k).or_insert(v);
    }
    env.insert("FERROBOT_AGENT_TOKEN".to_owned(), token);
    Ok(SandboxSpec {
        name: agent.sandbox_name.clone(),
        env,
        labels: BTreeMap::from([("dev.ferrobot.agent-id".to_owned(), agent.id.to_string())]),
        image: crate::clis::image(state).await?,
        command: None,
    })
}

/// Writes the CA bundle an agent with a login needs into its home: the sandbox's own bundle plus
/// the login proxy's CA. A failure leaves its logins unusable, not the agent.
async fn trust_login_ca(state: &AppState, agent: &Agent, handle: &SandboxHandle) {
    use crate::logins::{BUNDLE_FILE, CA_FILE, SYSTEM_BUNDLE};
    let home = state.backend.home();
    let path = |p: &str| ferrobot_sandbox::SandboxPath::parse(p).map_err(|e| e.to_string());
    let written = async {
        let mkdir =
            ferrobot_sandbox::ExecRequest::new(["mkdir", "-p", &format!("{home}/.ferrobot")]);
        state
            .backend
            .exec(handle, &mkdir)
            .await
            .map_err(|e| e.to_string())?;
        let system = state.backend.read_file(handle, &path(SYSTEM_BUNDLE)?).await;
        let system = String::from_utf8_lossy(&system.unwrap_or_default()).into_owned();
        let logins = crate::logins::of_agent(state, agent.id).await;
        let (ca, bundle) = crate::egress::trust_files(state, &system, &logins).await?;
        let ca_path = path(&format!("{home}/{CA_FILE}"))?;
        let bundle_path = path(&format!("{home}/{BUNDLE_FILE}"))?;
        state
            .backend
            .write_file(handle, &ca_path, ca.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        state
            .backend
            .write_file(handle, &bundle_path, bundle.as_bytes())
            .await
            .map_err(|e| e.to_string())
    };
    if let Err(e) = written.await {
        tracing::warn!(error = %e, "the login CA could not be put in the sandbox");
    }
}

async fn set_status(state: &AppState, id: AgentId, status: AgentStatus) -> Result<(), ApiError> {
    state.store.set_agent_status(id, status).await?;
    state.publish_agent(id).await;
    Ok(())
}

async fn adopt(state: &AppState, id: AgentId, handle: &SandboxHandle) -> Result<(), ApiError> {
    state
        .store
        .set_agent_sandbox(id, handle.backend.clone(), handle.id.clone())
        .await?;
    state.hub.mark_started(id);
    set_status(state, id, AgentStatus::Running).await
}

/// Issues a fresh token and creates + starts the agent's sandbox.
pub async fn provision(state: &AppState, agent: &Agent) -> Result<SandboxHandle, ApiError> {
    let _guard = state.hub.lifecycle_lock(agent.id).await;
    provision_locked(state, agent).await
}

async fn provision_locked(state: &AppState, agent: &Agent) -> Result<SandboxHandle, ApiError> {
    let spec = fresh_spec(state, agent).await?;
    match state.backend.create(&spec).await {
        Ok(handle) => {
            adopt(state, agent.id, &handle).await?;
            if spec.env.contains_key("HTTPS_PROXY") {
                trust_login_ca(state, agent, &handle).await;
            }
            if crate::clis::has_home(state, agent.id).await {
                state.clis.take_pending(agent.id);
                install_home(state, agent.id);
            }
            default_skills(state, &handle).await;
            Ok(handle)
        }
        Err(e) => {
            tracing::error!(agent = %agent.name, error = %e, "sandbox provisioning failed");
            set_status(state, agent.id, AgentStatus::Failed).await?;
            Err(e.into())
        }
    }
}

/// The skills every agent starts with, pinned: (name, file, contents). `find-skills` is
/// vercel-labs/skills at e7fd0e9 (MIT), so agents can look for more on skills.sh.
const DEFAULT_SKILLS: [(&str, &str, &[u8]); 2] = [
    (
        "find-skills",
        "SKILL.md",
        include_bytes!("skills/find-skills/SKILL.md"),
    ),
    (
        "find-skills",
        "LICENSE",
        include_bytes!("skills/find-skills/LICENSE"),
    ),
];
/// Bumped when a default skill changes, so computers that have an older copy get the new one.
const DEFAULT_SKILLS_VERSION: &str = "find-skills@e7fd0e9";

/// Puts the default skills into `~/.agents/skills` unless this version is already there. A
/// failure leaves the agent without them, not without its computer.
async fn default_skills(state: &AppState, handle: &SandboxHandle) {
    let home = state.backend.home();
    let root = format!("{home}/.agents/skills");
    let marker = format!("{root}/.ferrobot-defaults");
    let check = ferrobot_sandbox::ExecRequest::new(["/bin/cat", marker.as_str()]);
    if state
        .backend
        .exec(handle, &check)
        .await
        .is_ok_and(|out| out.stdout.trim() == DEFAULT_SKILLS_VERSION)
    {
        return;
    }
    let written = async {
        let path = |p: &str| ferrobot_sandbox::SandboxPath::parse(p).map_err(|e| e.to_string());
        let dirs: Vec<String> = DEFAULT_SKILLS
            .iter()
            .map(|(name, ..)| format!("{root}/{name}"))
            .collect();
        let mut mkdir = vec!["/bin/mkdir".to_owned(), "-p".to_owned()];
        mkdir.extend(dirs);
        state
            .backend
            .exec(handle, &ferrobot_sandbox::ExecRequest::new(mkdir))
            .await
            .map_err(|e| e.to_string())?;
        for (name, file, bytes) in DEFAULT_SKILLS {
            let target = path(&format!("{root}/{name}/{file}"))?;
            state
                .backend
                .write_file(handle, &target, bytes)
                .await
                .map_err(|e| e.to_string())?;
        }
        state
            .backend
            .write_file(handle, &path(&marker)?, DEFAULT_SKILLS_VERSION.as_bytes())
            .await
            .map_err(|e| e.to_string())
    };
    if let Err(e) = written.await {
        tracing::warn!(error = %e, "the default skills could not be put in the sandbox");
    }
}

/// Installs the agent's home-folder CLIs in the background; its first turn may come before.
fn install_home(state: &AppState, agent: AgentId) {
    let state = state.clone();
    tokio::spawn(async move { crate::clis::sync_home(&state, agent).await });
}

/// Starts the agent's sandbox, provisioning it if it does not exist (anymore) or runs an
/// outdated image: other CLIs, or a rebuilt runner under the same name (the disk is kept).
pub async fn wake(state: &AppState, id: AgentId) -> Result<(), ApiError> {
    let _guard = state.hub.lifecycle_lock(id).await;
    let agent = state.store.get_agent(id).await?;
    let Some(handle) = handle_of(&agent) else {
        provision_locked(state, &agent).await?;
        return Ok(());
    };
    let image = crate::clis::image(state).await?;
    if state.backend.is_stale(&handle, image.as_deref()).await? {
        tracing::info!(agent = %agent.name, "new sandbox image, recreating the sandbox");
        state.backend.destroy(&handle, false).await?;
        return provision_locked(state, &agent).await.map(|_| ());
    }
    match state.backend.wake(&handle).await {
        Ok(()) => {
            if state.clis.take_pending(id) {
                install_home(state, id);
            }
            default_skills(state, &handle).await;
            if agent.status != AgentStatus::Running {
                state.record(id, None, "awake", json!({})).await;
            }
            state.hub.mark_started(id);
            set_status(state, id, AgentStatus::Running).await
        }
        Err(SandboxError::NotFound(_)) => {
            tracing::info!(agent = %agent.name, "sandbox missing, re-provisioning");
            provision_locked(state, &agent).await.map(|_| ())
        }
        Err(e) => Err(e.into()),
    }
}

/// Wakes the agent unless it is already meant to be running.
pub async fn ensure_awake(state: &AppState, id: AgentId) -> Result<(), ApiError> {
    let agent = state.store.get_agent(id).await?;
    if agent.status == AgentStatus::Running {
        return Ok(());
    }
    wake(state, id).await
}

/// Puts the agent to sleep: stops the sandbox, keeping its disk. Queued
/// messages wait; a new message wakes it again.
pub async fn sleep(
    state: &AppState,
    id: AgentId,
    reason: &str,
    level: SleepLevel,
) -> Result<(), ApiError> {
    let _guard = state.hub.lifecycle_lock(id).await;
    let agent = state.store.get_agent(id).await?;
    let mut reached = SleepLevel::Cold;
    if let Some(handle) = handle_of(&agent) {
        match state.backend.sleep(&handle, level).await {
            Ok(level) => reached = level,
            Err(SandboxError::NotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let status = match reached {
        SleepLevel::Warm => AgentStatus::Paused,
        SleepLevel::Cold => AgentStatus::Stopped,
        SleepLevel::Archived => {
            // The archived image still carries the old token in its env: retire it.
            state.store.set_agent_token(id, &new_token()).await?;
            AgentStatus::Archived
        }
    };
    if agent.status != status {
        state
            .record(
                id,
                None,
                "sleeping",
                json!({ "reason": reason, "level": reached }),
            )
            .await;
    }
    set_status(state, id, status).await
}

/// Destroys the sandbox and removes the agent with all its data.
pub async fn delete(state: &AppState, id: AgentId, purge_disk: bool) -> Result<(), ApiError> {
    let _guard = state.hub.lifecycle_lock(id).await;
    let agent = state.store.get_agent(id).await?;
    if let Some(handle) = handle_of(&agent) {
        state.backend.destroy(&handle, purge_disk).await?;
    }
    if purge_disk {
        state.backend.delete_archive(&agent.sandbox_name).await?;
        for snap in state.store.list_snapshots(id).await? {
            if let Err(e) = state.backend.delete_snapshot(&snap.snapshot_ref).await {
                tracing::warn!(snapshot = %snap.snapshot_ref, error = %e, "cannot delete snapshot");
            }
        }
    }
    state.store.delete_agent(id).await?;
    crate::claude_code::forget(state, id).await;
    state.hub.forget(id);
    state.publish(UiEvent::AgentDeleted { agent_id: id });
    Ok(())
}

/// Removes the sandbox but keeps the agent and its disk; `wake` recreates it.
pub async fn destroy_sandbox(state: &AppState, id: AgentId) -> Result<(), ApiError> {
    let _guard = state.hub.lifecycle_lock(id).await;
    let agent = state.store.get_agent(id).await?;
    let handle = sandbox_handle(&agent)?;
    state.backend.destroy(&handle, false).await?;
    set_status(state, id, AgentStatus::Stopped).await
}

/// Snapshots the agent's sandbox and records it.
pub async fn snapshot(
    state: &AppState,
    id: AgentId,
    label: &str,
) -> Result<SnapshotRecord, ApiError> {
    let _guard = state.hub.lifecycle_lock(id).await;
    let agent = state.store.get_agent(id).await?;
    let handle = sandbox_handle(&agent)?;
    let snap = state.backend.snapshot(&handle, label).await?;
    let record = state
        .store
        .add_snapshot(id, handle.backend, snap.id, label.to_owned(), snap.scope)
        .await?;
    state
        .record(
            id,
            None,
            "snapshot",
            json!({ "snapshot_id": record.id, "label": label }),
        )
        .await;
    Ok(record)
}

/// Rebuilds the sandbox (disk kept) so a new provider or model takes
/// effect; a running agent comes back up right away.
pub async fn recreate(state: &AppState, id: AgentId) -> Result<(), ApiError> {
    let _guard = state.hub.lifecycle_lock(id).await;
    let agent = state.store.get_agent(id).await?;
    let Some(handle) = handle_of(&agent) else {
        return Ok(());
    };
    state.backend.destroy(&handle, false).await?;
    // A sleeping agent is re-provisioned by its next wake (the backend reports NotFound).
    if agent.status == AgentStatus::Running {
        provision_locked(state, &agent).await?;
    }
    Ok(())
}

/// Replaces the sandbox with one booted from a snapshot.
pub async fn restore(state: &AppState, id: AgentId, snapshot_id: i64) -> Result<(), ApiError> {
    let _guard = state.hub.lifecycle_lock(id).await;
    let agent = state.store.get_agent(id).await?;
    let snap = owned_snapshot(state, id, snapshot_id).await?;
    let handle = sandbox_handle(&agent)?;
    if handle.backend != snap.backend {
        return Err(ApiError::Conflict(format!(
            "snapshot was taken on the {} backend",
            snap.backend
        )));
    }
    let spec = fresh_spec(state, &agent).await?;
    let new = state
        .backend
        .restore(&handle, &snap.snapshot_ref, &spec)
        .await?;
    // The restored disk carries its own runner session.
    adopt(state, id, &new).await?;
    state
        .record(
            id,
            None,
            "restored",
            json!({ "snapshot_id": snap.id, "label": snap.label }),
        )
        .await;
    Ok(())
}

pub async fn delete_snapshot(
    state: &AppState,
    id: AgentId,
    snapshot_id: i64,
) -> Result<(), ApiError> {
    let snap = owned_snapshot(state, id, snapshot_id).await?;
    state.backend.delete_snapshot(&snap.snapshot_ref).await?;
    state.store.delete_snapshot(snap.id).await?;
    Ok(())
}

async fn owned_snapshot(
    state: &AppState,
    agent: AgentId,
    snapshot_id: i64,
) -> Result<SnapshotRecord, ApiError> {
    let snap = state.store.get_snapshot(snapshot_id).await?;
    if snap.agent_id == agent {
        Ok(snap)
    } else {
        Err(ApiError::NotFound(format!(
            "snapshot {snapshot_id} not found"
        )))
    }
}

/// Brings sandboxes of `running` agents back after a daemon restart, aligns
/// stored status with what the backend reports and rebuilds the CLI image.
pub async fn reconcile(state: &AppState) {
    crate::clis::resume(state);
    let agents = match state.store.list_agents().await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "reconcile: cannot list agents");
            return;
        }
    };
    for agent in agents {
        let live = match handle_of(&agent) {
            None => None,
            Some(h) => state.backend.state(&h).await.ok(),
        };
        match (agent.status, live) {
            (AgentStatus::Running, Some(SandboxState::Running)) => {
                state.hub.mark_started(agent.id);
            }
            (AgentStatus::Running | AgentStatus::Created, _) => {
                if let Err(e) = wake(state, agent.id).await {
                    tracing::warn!(agent = %agent.name, error = %e, "reconcile: wake failed");
                }
            }
            (AgentStatus::Archived, _) => {
                let queued = state
                    .store
                    .has_waking_messages(agent.id)
                    .await
                    .unwrap_or(false);
                if queued && let Err(e) = wake(state, agent.id).await {
                    tracing::warn!(agent = %agent.name, error = %e, "reconcile: wake failed");
                }
            }
            (status @ (AgentStatus::Paused | AgentStatus::Stopped), live) => {
                let level = match status {
                    AgentStatus::Paused if live == Some(SandboxState::Paused) => None,
                    AgentStatus::Paused if live == Some(SandboxState::Running) => {
                        Some(SleepLevel::Warm)
                    }
                    AgentStatus::Paused => Some(SleepLevel::Cold),
                    _ if matches!(live, Some(SandboxState::Running | SandboxState::Paused)) => {
                        Some(SleepLevel::Cold)
                    }
                    _ => None,
                };
                let queued = state
                    .store
                    .has_waking_messages(agent.id)
                    .await
                    .unwrap_or(false);
                let result = match level {
                    _ if queued => wake(state, agent.id).await,
                    Some(level) => sleep(state, agent.id, "reconcile", level).await,
                    None => Ok(()),
                };
                if let Err(e) = result {
                    tracing::warn!(agent = %agent.name, error = %e, "reconcile failed");
                }
            }
            (AgentStatus::Failed, _) => {}
        }
    }
}
