//! API views that combine stored data with live daemon state.

use ferrobot_core::{Agent, AgentId, AgentStatus};
use serde::Serialize;
use utoipa::ToSchema;

use crate::AppState;
use crate::error::ApiError;

/// What an agent is doing right now, for status badges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    /// Running a turn.
    Working,
    /// Sandbox running and runner connected, idle.
    Awake,
    /// Sandbox starting or runner not connected yet.
    Starting,
    /// Sandbox stopped (manually or after idling); a new message wakes it.
    Sleeping,
    /// Provisioning failed.
    Error,
}

/// An agent plus live state.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentView {
    #[serde(flatten)]
    pub agent: Agent,
    /// Whether the agent's runner is connected right now.
    pub connected: bool,
    pub presence: Presence,
    /// Finished turns since the operator last marked the chat as read.
    pub unread: u32,
    /// Relative URL of the avatar image, if one was uploaded.
    pub avatar_url: Option<String>,
}

pub fn presence(agent: &Agent, connected: bool, working: bool) -> Presence {
    match agent.status {
        AgentStatus::Failed => Presence::Error,
        AgentStatus::Paused | AgentStatus::Stopped | AgentStatus::Archived => Presence::Sleeping,
        AgentStatus::Running if working => Presence::Working,
        AgentStatus::Running if connected => Presence::Awake,
        AgentStatus::Created | AgentStatus::Running => Presence::Starting,
    }
}

pub fn view(state: &AppState, agent: Agent, unread: u32) -> AgentView {
    let connected = state.hub.is_connected(agent.id);
    let working = state.hub.is_working(agent.id);
    AgentView {
        presence: presence(&agent, connected, working),
        avatar_url: (agent.avatar_rev > 0)
            .then(|| format!("/v1/agents/{}/avatar?v={}", agent.id, agent.avatar_rev)),
        connected,
        unread,
        agent,
    }
}

pub async fn agent_view(state: &AppState, id: AgentId) -> Result<AgentView, ApiError> {
    let agent = state.store.get_agent(id).await?;
    let unread = state
        .store
        .unread_counts()
        .await?
        .get(&id)
        .copied()
        .unwrap_or(0);
    Ok(view(state, agent, unread))
}

pub async fn all_agents(state: &AppState) -> Result<Vec<AgentView>, ApiError> {
    let unread = state.store.unread_counts().await?;
    Ok(state
        .store
        .list_agents()
        .await?
        .into_iter()
        .map(|a| {
            let n = unread.get(&a.id).copied().unwrap_or(0);
            view(state, a, n)
        })
        .collect())
}
