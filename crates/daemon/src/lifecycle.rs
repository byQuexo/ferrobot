//! Agent sandbox lifecycle: provision, wake, sleep, restore, destroy, reconcile.
//!
//! Status semantics: `running` means the agent should be awake (the
//! supervisor restarts a crashed sandbox), `stopped` means it sleeps until
//! the next message or an explicit wake.

use std::collections::BTreeMap;

use ferrobot_core::{Agent, AgentId, AgentStatus};
use ferrobot_sandbox::{SandboxError, SandboxHandle, SandboxSpec, SandboxState};
use ferrobot_store::SnapshotRecord;
use serde_json::json;

use crate::error::ApiError;
use crate::{AppState, UiEvent};

/// 256-bit random bearer token (two v4 UUIDs, hex encoded).
fn new_token() -> String {
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
    let mut env = BTreeMap::from([
        (
            "FERROBOT_DAEMON_URL".to_owned(),
            s.sandbox_daemon_url.clone(),
        ),
        ("FERROBOT_AGENT_NAME".to_owned(), agent.name.to_string()),
        ("FERROBOT_DRIVER".to_owned(), s.driver.clone()),
    ]);
    if s.driver == "claude" {
        // The real key never enters the sandbox: the SDK talks to the
        // daemon's model proxy and authenticates with the agent token.
        env.insert(
            "ANTHROPIC_BASE_URL".to_owned(),
            format!("{}/model", s.sandbox_daemon_url.trim_end_matches('/')),
        );
        env.insert("ANTHROPIC_API_KEY".to_owned(), token.clone());
        env.insert(
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_owned(),
            "1".to_owned(),
        );
        if let Some(model) = &s.model {
            env.insert("FERROBOT_MODEL".to_owned(), model.clone());
        }
    }
    env.insert("FERROBOT_AGENT_TOKEN".to_owned(), token);
    Ok(SandboxSpec {
        name: agent.sandbox_name.clone(),
        env,
        labels: BTreeMap::from([("dev.ferrobot.agent-id".to_owned(), agent.id.to_string())]),
    })
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
    let lock = state.hub.lifecycle_lock(agent.id);
    let _guard = lock.lock().await;
    provision_locked(state, agent).await
}

async fn provision_locked(state: &AppState, agent: &Agent) -> Result<SandboxHandle, ApiError> {
    let spec = fresh_spec(state, agent).await?;
    match state.backend.create(&spec).await {
        Ok(handle) => {
            adopt(state, agent.id, &handle).await?;
            Ok(handle)
        }
        Err(e) => {
            tracing::error!(agent = %agent.name, error = %e, "sandbox provisioning failed");
            set_status(state, agent.id, AgentStatus::Failed).await?;
            Err(e.into())
        }
    }
}

/// Starts the agent's sandbox, provisioning it if it does not exist (anymore).
pub async fn wake(state: &AppState, id: AgentId) -> Result<(), ApiError> {
    let lock = state.hub.lifecycle_lock(id);
    let _guard = lock.lock().await;
    let agent = state.store.get_agent(id).await?;
    let Some(handle) = handle_of(&agent) else {
        provision_locked(state, &agent).await?;
        return Ok(());
    };
    match state.backend.wake(&handle).await {
        Ok(()) => {
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
pub async fn sleep(state: &AppState, id: AgentId, reason: &str) -> Result<(), ApiError> {
    let lock = state.hub.lifecycle_lock(id);
    let _guard = lock.lock().await;
    let agent = state.store.get_agent(id).await?;
    if let Some(handle) = handle_of(&agent) {
        match state.backend.stop(&handle).await {
            Ok(()) | Err(SandboxError::NotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    if agent.status != AgentStatus::Stopped {
        state
            .record(id, None, "sleeping", json!({ "reason": reason }))
            .await;
    }
    set_status(state, id, AgentStatus::Stopped).await
}

/// Destroys the sandbox and removes the agent with all its data.
pub async fn delete(state: &AppState, id: AgentId, purge_disk: bool) -> Result<(), ApiError> {
    let lock = state.hub.lifecycle_lock(id);
    let _guard = lock.lock().await;
    let agent = state.store.get_agent(id).await?;
    if let Some(handle) = handle_of(&agent) {
        state.backend.destroy(&handle, purge_disk).await?;
    }
    if purge_disk {
        for snap in state.store.list_snapshots(id).await? {
            if let Err(e) = state.backend.delete_snapshot(&snap.snapshot_ref).await {
                tracing::warn!(snapshot = %snap.snapshot_ref, error = %e, "cannot delete snapshot");
            }
        }
    }
    state.store.delete_agent(id).await?;
    state.hub.forget(id);
    state.publish(UiEvent::AgentDeleted { agent_id: id });
    Ok(())
}

/// Removes the sandbox but keeps the agent and its disk; `wake` recreates it.
pub async fn destroy_sandbox(state: &AppState, id: AgentId) -> Result<(), ApiError> {
    let lock = state.hub.lifecycle_lock(id);
    let _guard = lock.lock().await;
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
    let lock = state.hub.lifecycle_lock(id);
    let _guard = lock.lock().await;
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

/// Replaces the sandbox with one booted from a snapshot.
pub async fn restore(state: &AppState, id: AgentId, snapshot_id: i64) -> Result<(), ApiError> {
    let lock = state.hub.lifecycle_lock(id);
    let _guard = lock.lock().await;
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

/// Brings sandboxes of `running` agents back after a daemon restart and
/// aligns stored status with what the backend reports.
pub async fn reconcile(state: &AppState) {
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
            // Sleeping agents with something queued get woken; otherwise make
            // sure a sandbox that kept running is put back to sleep.
            (AgentStatus::Stopped, live) => {
                let queued = state
                    .store
                    .has_queued_messages(agent.id)
                    .await
                    .unwrap_or(false);
                let result = if queued {
                    wake(state, agent.id).await
                } else if live == Some(SandboxState::Running) {
                    sleep(state, agent.id, "reconcile").await
                } else {
                    Ok(())
                };
                if let Err(e) = result {
                    tracing::warn!(agent = %agent.name, error = %e, "reconcile failed");
                }
            }
            (AgentStatus::Failed, _) => {}
        }
    }
}
