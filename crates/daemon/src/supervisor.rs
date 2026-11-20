//! Background care for sandboxes: idle auto-sleep (warm, then cold) and
//! crash recovery.

use std::time::{Duration, Instant};

use ferrobot_core::{Agent, AgentStatus};
use ferrobot_sandbox::{SandboxError, SandboxState, SleepLevel};
use serde_json::json;

use crate::AppState;
use crate::error::ApiError;
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
    for agent in agents.into_iter().filter(|a| {
        matches!(a.status, AgentStatus::Running | AgentStatus::Paused)
            || (a.status == AgentStatus::Stopped && state.settings.archive_timeout.is_some())
    }) {
        if let Err(e) = check(state, &agent).await {
            tracing::warn!(agent = %agent.name, error = %e, "supervisor check failed");
        }
    }
    crate::tasks::nudge(state).await;
}

async fn check(state: &AppState, agent: &Agent) -> Result<(), crate::error::ApiError> {
    let id = agent.id;
    let Some(handle) = handle_of(agent) else {
        return Ok(());
    };
    let idle = Instant::now().duration_since(state.hub.last_activity(id));
    if agent.status == AgentStatus::Stopped {
        if state.settings.archive_timeout.is_some_and(|t| idle >= t)
            && !state.store.has_queued_messages(id).await?
        {
            tracing::info!(agent = %agent.name, "idle, archiving");
            match lifecycle::sleep(state, id, "idle", SleepLevel::Archived).await {
                // Backends without archive support keep the stopped sandbox.
                Err(ApiError::Sandbox(SandboxError::NotImplemented { .. })) => {}
                other => other?,
            }
        }
        return Ok(());
    }
    if agent.status == AgentStatus::Paused {
        // A paused sandbox is asleep on purpose, never a crash; it only goes colder.
        if state.settings.idle_timeout.is_some_and(|t| idle >= t)
            && !state.store.has_queued_messages(id).await?
        {
            tracing::info!(agent = %agent.name, "idle, going to cold sleep");
            lifecycle::sleep(state, id, "idle", SleepLevel::Cold).await?;
        }
        return Ok(());
    }
    // Crash recovery: the agent should be awake but its sandbox is not running.
    match state.backend.state(&handle).await? {
        SandboxState::Running => {}
        live @ (SandboxState::Paused | SandboxState::Stopped | SandboxState::Missing) => {
            tracing::warn!(agent = %agent.name, ?live, "sandbox down, restarting");
            state
                .record(id, None, "sandbox_restarted", json!({ "was": live }))
                .await;
            return lifecycle::wake(state, id).await;
        }
    }
    let working = state.hub.is_working(id);
    let connected = state.hub.is_connected(id);
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
    // Idle auto-sleep: the deepest level whose timeout has passed.
    let s = &state.settings;
    let level = if s.idle_timeout.is_some_and(|t| idle >= t) {
        Some(SleepLevel::Cold)
    } else if s.warm_timeout.is_some_and(|t| idle >= t) {
        Some(SleepLevel::Warm)
    } else {
        None
    };
    if let Some(level) = level
        && !working
        && !state.store.has_queued_messages(id).await?
    {
        tracing::info!(agent = %agent.name, ?level, "idle, going to sleep");
        match lifecycle::sleep(state, id, "idle", level).await {
            // Backends that cannot pause keep the agent awake until the cold timeout.
            Err(ApiError::Sandbox(SandboxError::NotImplemented { .. }))
                if level == SleepLevel::Warm => {}
            other => other?,
        }
    }
    Ok(())
}
