//! Background care for sandboxes: idle auto-sleep and crash recovery.

use std::time::{Duration, Instant};

use ferrobot_core::{Agent, AgentStatus};
use ferrobot_sandbox::SandboxState;
use serde_json::json;

use crate::AppState;
use crate::lifecycle::{self, handle_of};

/// A running sandbox whose runner has not connected after this long gets restarted.
const CONNECT_GRACE: Duration = Duration::from_secs(120);

/// Runs forever, checking every agent each `settings.supervisor_interval`.
pub async fn run(state: AppState) {
    let mut tick = tokio::time::interval(state.settings.supervisor_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        check_all(&state).await;
    }
}

/// One supervision pass over all agents (public for tests).
pub async fn check_all(state: &AppState) {
    let agents = match state.store.list_agents().await {
        Ok(agents) => agents,
        Err(e) => {
            tracing::error!(error = %e, "supervisor: cannot list agents");
            return;
        }
    };
    for agent in agents
        .into_iter()
        .filter(|a| a.status == AgentStatus::Running)
    {
        if let Err(e) = check(state, &agent).await {
            tracing::warn!(agent = %agent.name, error = %e, "supervisor check failed");
        }
    }
}

async fn check(state: &AppState, agent: &Agent) -> Result<(), crate::error::ApiError> {
    let id = agent.id;
    let Some(handle) = handle_of(agent) else {
        return Ok(());
    };
    // Crash recovery: the agent should be awake but its sandbox is not running.
    match state.backend.state(&handle).await? {
        SandboxState::Running => {}
        live @ (SandboxState::Stopped | SandboxState::Missing) => {
            tracing::warn!(agent = %agent.name, ?live, "sandbox down, restarting");
            state
                .record(id, None, "sandbox_restarted", json!({ "was": live }))
                .await;
            return lifecycle::wake(state, id).await;
        }
    }
    let working = state.hub.is_working(id);
    let connected = state.hub.is_connected(id);
    // Health: a running sandbox whose runner never connected is restarted.
    if !connected
        && state
            .hub
            .started_at(id)
            .is_some_and(|t| t.elapsed() > CONNECT_GRACE)
    {
        tracing::warn!(agent = %agent.name, "runner did not connect, restarting sandbox");
        state
            .record(
                id,
                None,
                "sandbox_restarted",
                json!({ "was": "unresponsive" }),
            )
            .await;
        state.backend.stop(&handle).await?;
        return lifecycle::wake(state, id).await;
    }
    // Idle auto-sleep.
    if let Some(timeout) = state.settings.idle_timeout
        && !working
        && Instant::now().duration_since(state.hub.last_activity(id)) >= timeout
        && !state.store.has_queued_messages(id).await?
    {
        tracing::info!(agent = %agent.name, "idle, going to sleep");
        lifecycle::sleep(state, id, "idle").await?;
    }
    Ok(())
}
