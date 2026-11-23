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
    /// Sandbox paused, stopped or archived; a new message wakes it.
    Sleeping,
    /// Provisioning failed.
    Error,
}

/// What is wrong with an agent's model credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialProblem {
    /// No key or sign-in to call the model with.
    Missing,
    /// The provider refused the key or sign-in.
    Rejected,
}

/// A model credential problem found by the model proxy. The agent's messages wait until the
/// operator changes its provider or model, stores a key or retries. Kept in memory: after a
/// restart the next model call finds it again.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CredentialError {
    pub kind: CredentialProblem,
    /// What is wrong and where to fix it; never contains the key.
    pub reason: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub since: time::OffsetDateTime,
}

/// The agent's model refused a request with tools. The agent's messages wait until the operator
/// approves the card to run it without tools, changes its provider or model, or retries. Kept in
/// memory: after a restart the next model call finds it again.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ToolsUnsupported {
    /// The model as the agent requested it.
    pub model: String,
    /// Which provider can't run the model with tools; never the upstream's words.
    pub reason: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub since: time::OffsetDateTime,
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
    /// Set while the agent's model credential is missing or rejected; its turns wait.
    pub credential_error: Option<CredentialError>,
    /// Set while the agent's model can't use tools and the operator has not turned them off;
    /// its turns wait.
    pub tools_unsupported: Option<ToolsUnsupported>,
    /// Question cards waiting for the operator's answer.
    pub waiting: u32,
    /// The agent's newest reply, cut to a preview.
    pub last_reply: Option<LastReply>,
}

/// The start of an agent's newest reply.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LastReply {
    /// At most 160 characters.
    pub text: String,
    /// When the reply came (RFC 3339).
    pub at: String,
}

/// Per-agent counts the agent list shows, read once for all agents.
#[derive(Debug, Default)]
pub struct Counts {
    unread: std::collections::HashMap<AgentId, u32>,
    waiting: std::collections::HashMap<AgentId, u32>,
    replies: std::collections::HashMap<AgentId, (String, String)>,
}

impl Counts {
    pub async fn load(state: &AppState) -> Result<Self, ApiError> {
        Ok(Self {
            unread: state.store.unread_counts().await?,
            waiting: state.store.waiting_counts().await?,
            replies: state.store.last_replies().await?,
        })
    }
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

pub fn view(state: &AppState, agent: Agent, counts: &Counts) -> AgentView {
    let connected = state.hub.is_connected(agent.id);
    let working = state.hub.is_working(agent.id);
    let id = agent.id;
    AgentView {
        unread: counts.unread.get(&id).copied().unwrap_or(0),
        waiting: counts.waiting.get(&id).copied().unwrap_or(0),
        last_reply: counts.replies.get(&id).map(|(text, at)| LastReply {
            text: text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(160)
                .collect(),
            at: at.clone(),
        }),
        presence: presence(&agent, connected, working),
        avatar_url: (agent.avatar_rev > 0)
            .then(|| format!("/v1/agents/{}/avatar?v={}", agent.id, agent.avatar_rev)),
        connected,
        credential_error: state.hub.credential_error(agent.id),
        tools_unsupported: state.hub.tools_unsupported(agent.id),
        agent,
    }
}

pub async fn agent_view(state: &AppState, id: AgentId) -> Result<AgentView, ApiError> {
    let agent = state.store.get_agent(id).await?;
    Ok(view(state, agent, &Counts::load(state).await?))
}

pub async fn all_agents(state: &AppState) -> Result<Vec<AgentView>, ApiError> {
    let counts = Counts::load(state).await?;
    Ok(state
        .store
        .list_agents()
        .await?
        .into_iter()
        .map(|a| view(state, a, &counts))
        .collect())
}
