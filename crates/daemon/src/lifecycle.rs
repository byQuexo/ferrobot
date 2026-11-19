//! Agent sandbox lifecycle: provision, wake, stop, destroy, reconcile.

use std::collections::BTreeMap;

use ferrobot_core::{Agent, AgentId, AgentStatus};
use ferrobot_sandbox::{SandboxError, SandboxHandle, SandboxSpec, SandboxState};

use crate::AppState;
use crate::error::ApiError;

/// 256-bit random bearer token (two v4 UUIDs, hex encoded).
fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn handle_of(agent: &Agent) -> Option<SandboxHandle> {
    Some(SandboxHandle {
        backend: agent.backend.clone()?,
        id: agent.sandbox_id.clone()?,
    })
}

/// Issues a fresh token and creates + starts the agent's sandbox.
pub async fn provision(state: &AppState, agent: &Agent) -> Result<SandboxHandle, ApiError> {
    let token = new_token();
    state.store.set_agent_token(agent.id, &token).await?;

    let s = &state.settings;
    let mut env = BTreeMap::from([
        ("FERROBOT_DAEMON_URL".to_owned(), s.sandbox_daemon_url.clone()),
        ("FERROBOT_AGENT_TOKEN".to_owned(), token),
        ("FERROBOT_AGENT_NAME".to_owned(), agent.name.to_string()),
        ("FERROBOT_DRIVER".to_owned(), s.driver.clone()),
    ]);
    if s.driver == "claude" {
        if let Some(key) = &s.anthropic_api_key {
            env.insert("ANTHROPIC_API_KEY".to_owned(), key.clone());
        }
        if let Some(model) = &s.model {
            env.insert("FERROBOT_MODEL".to_owned(), model.clone());
        }
    }
    let spec = SandboxSpec {
        name: agent.name.to_string(),
        env,
        labels: BTreeMap::from([("dev.ferrobot.agent-id".to_owned(), agent.id.to_string())]),
    };

    match state.backend.create(&spec).await {
        Ok(handle) => {
            state
                .store
                .set_agent_sandbox(agent.id, handle.backend.clone(), handle.id.clone())
                .await?;
            state.store.set_agent_status(agent.id, AgentStatus::Running).await?;
            state.publish_agent(agent.id).await;
            Ok(handle)
        }
        Err(e) => {
            tracing::error!(agent = %agent.name, error = %e, "sandbox provisioning failed");
            state.store.set_agent_status(agent.id, AgentStatus::Failed).await?;
            state.publish_agent(agent.id).await;
            Err(e.into())
        }
    }
}

/// Starts the agent's sandbox, provisioning it if it does not exist (anymore).
pub async fn wake(state: &AppState, id: AgentId) -> Result<(), ApiError> {
    let agent = state.store.get_agent(id).await?;
    let Some(handle) = handle_of(&agent) else {
        provision(state, &agent).await?;
        return Ok(());
    };
    match state.backend.wake(&handle).await {
        Ok(()) => {
            state.store.set_agent_status(id, AgentStatus::Running).await?;
            state.publish_agent(id).await;
            Ok(())
        }
        Err(SandboxError::NotFound(_)) => {
            tracing::info!(agent = %agent.name, "sandbox missing, re-provisioning");
            provision(state, &agent).await.map(|_| ())
        }
        Err(e) => Err(e.into()),
    }
}

/// Stops the sandbox (disk kept). Queued messages wait until the next wake.
pub async fn stop(state: &AppState, id: AgentId) -> Result<(), ApiError> {
    let agent = state.store.get_agent(id).await?;
    if let Some(handle) = handle_of(&agent) {
        state.backend.stop(&handle).await?;
    }
    state.store.set_agent_status(id, AgentStatus::Stopped).await?;
    state.publish_agent(id).await;
    Ok(())
}

/// Destroys the sandbox and removes the agent with all its data.
pub async fn delete(state: &AppState, id: AgentId, purge_disk: bool) -> Result<(), ApiError> {
    let agent = state.store.get_agent(id).await?;
    if let Some(handle) = handle_of(&agent) {
        state.backend.destroy(&handle, purge_disk).await?;
    }
    state.store.delete_agent(id).await?;
    state.hub.forget(id);
    state.publish(crate::UiEvent::AgentDeleted { agent_id: id });
    Ok(())
}

pub fn sandbox_handle(agent: &Agent) -> Result<SandboxHandle, ApiError> {
    handle_of(agent).ok_or_else(|| ApiError::Conflict(format!("agent {} has no sandbox", agent.name)))
}

/// Brings sandboxes of `running` agents back after a daemon restart.
pub async fn reconcile(state: &AppState) {
    let agents = match state.store.list_agents().await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "reconcile: cannot list agents");
            return;
        }
    };
    for agent in agents.into_iter().filter(|a| a.status == AgentStatus::Running) {
        let needs_wake = match handle_of(&agent) {
            None => true,
            Some(h) => !matches!(state.backend.state(&h).await, Ok(SandboxState::Running)),
        };
        if needs_wake {
            if let Err(e) = wake(state, agent.id).await {
                tracing::warn!(agent = %agent.name, error = %e, "reconcile: wake failed");
            }
        }
    }
}
