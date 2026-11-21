//! Model credential problems per agent: the model proxy finds them, the dispatch loop holds the
//! agent's turns while one stands, and a change that may fix it clears it.

use ferrobot_core::AgentId;
use serde_json::json;

use crate::AppState;
use crate::views::{CredentialError, CredentialProblem};

/// Marks the agent's credential as broken; the first report leaves one notice in its chat.
/// Synchronous with the proxy's answer, so the failing turn already sees it when it settles.
pub async fn set(state: &AppState, agent: AgentId, kind: CredentialProblem, reason: String) {
    let error = CredentialError {
        kind,
        reason: reason.clone(),
        since: time::OffsetDateTime::now_utc(),
    };
    if !state.hub.set_credential_error(agent, error) {
        return;
    }
    tracing::warn!(%agent, ?kind, "model credential problem, turns held");
    // No event turn id: a room turn's events stay out of the chat, and this notice belongs there.
    let payload =
        json!({ "kind": kind, "reason": reason, "turn_id": state.hub.current_turn(agent) });
    state.record(agent, None, "credential_error", payload).await;
    state.publish_agent(agent).await;
}

/// Clears the agent's credential problem and runs its waiting messages; if the fix did not
/// work, the next model call sets it again.
pub async fn clear(state: &AppState, agent: AgentId) {
    if !state.hub.clear_credential_error(agent) {
        return;
    }
    state.publish_agent(agent).await;
    // Held turns let the agent idle into sleep: wake it when something waits.
    if matches!(state.store.has_queued_messages(agent).await, Ok(true)) {
        state.message_arrived(agent);
    } else {
        state.hub.wake(agent);
    }
}

/// Clears the problem of every agent on `provider`.
pub async fn clear_provider(state: &AppState, provider: uuid::Uuid) {
    let Ok(agents) = state.store.list_agents().await else {
        return;
    };
    for agent in agents.iter().filter(|a| a.provider_id == Some(provider)) {
        clear(state, agent.id).await;
    }
}
