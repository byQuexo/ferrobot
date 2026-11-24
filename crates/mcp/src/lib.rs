//! ferrobot's own tools as an MCP server (Streamable HTTP). [`router`] resolves the bearer
//! runner token to a [`Caller`] that tools act as, so an agent can only act as itself.

mod auth;
mod tools;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ferrobot_core::{AgentId, AvatarCharacter, MailMessage};
use ferrobot_store::{Store, StoredEvent};

pub use auth::{Caller, router};
pub use tools::FerrobotTools;

/// MCP server name as seen by clients. Claude exposes tools as `mcp__<name>__<tool>`.
pub const SERVER_NAME: &str = "ferrobot";

/// Upper bound for one message body (bytes).
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024;

/// Upper bound for a memory key (bytes).
pub const MAX_MEMORY_KEY_BYTES: usize = 128;

/// A boxed future, as returned by [`MailboxNotifier::request_agent`].
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// An agent's request to create another agent (`create_agent` tool).
#[derive(Debug, Clone)]
pub struct AgentRequest {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub character: Option<AvatarCharacter>,
    /// At most the requester's own; `None`: the same as its own.
    pub tools: Option<Vec<ferrobot_core::ToolGroup>>,
    /// Names of the requester's connectors to give it; `None`: all of them.
    pub connectors: Option<Vec<String>>,
}

/// An agent's request to add a connector (`add_connector` tool).
#[derive(Debug, Clone)]
pub struct ConnectorRequest {
    pub name: String,
    pub url: String,
    /// A local server instead of `url`: its program and arguments.
    pub command: Option<Vec<String>>,
    pub token_env: Option<String>,
    pub auth: ferrobot_core::ConnectorAuth,
    /// Why the agent wants it, shown on the card.
    pub reason: String,
}

/// An agent's request for a routine of its own (`create_routine` tool).
#[derive(Debug, Clone)]
pub struct RoutineRequest {
    pub name: String,
    /// 5-field cron expression or alias, in the daemon's time zone.
    pub schedule: String,
    pub prompt: String,
}

/// How a [`RoutineRequest`] ended for now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineRequestOutcome {
    /// Auto mode: the routine exists.
    Created { id: i64, when: String },
    /// The operator decides on a card; the outcome arrives as a message.
    Pending { question_id: i64 },
}

/// An agent's request to change the tools of an agent it created (`set_agent_tools` tool).
#[derive(Debug, Clone)]
pub struct AgentToolsRequest {
    pub agent: AgentId,
    /// Replaces the target's tool groups; `None` keeps them.
    pub tools: Option<Vec<ferrobot_core::ToolGroup>>,
    /// Names of the requester's connectors that replace the target's; `None` keeps them.
    pub connectors: Option<Vec<String>>,
}

/// How an [`AgentToolsRequest`] ended for now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentToolsOutcome {
    /// Narrower, or Auto mode: the change is made.
    Applied,
    /// Wider: the operator decides on a card; the outcome arrives as a message.
    Pending { question_id: i64 },
}

/// A file an agent shared with the operator (`share_file` tool).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFile {
    /// Id of the `file` event in the agent's chat.
    pub id: i64,
    pub name: String,
    pub size: u64,
}

/// How a [`AgentRequest`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRequestOutcome {
    /// The operator approved; the agent exists and is starting.
    Created { id: AgentId, name: String },
    /// The operator declined, or did not answer in time.
    Declined { reason: String },
}

/// Hooks the daemon uses to react to tool calls.
pub trait MailboxNotifier: Send + Sync + 'static {
    /// A tool enqueued a message, so the daemon can dispatch it (and wake
    /// the recipient unless `message.priority == Some(false)`).
    fn message_enqueued(&self, message: &MailMessage);
    /// A tool changed the agent's memory.
    fn memory_changed(&self, agent: AgentId) {
        let _ = agent;
    }
    /// A tool changed the global memory.
    fn global_memory_changed(&self) {}
    /// Tools of the connectors `agent` may use, named `<connector>__<tool>`.
    fn connector_tools(&self, agent: AgentId) -> BoxFuture<Vec<rmcp::model::Tool>> {
        let _ = agent;
        Box::pin(async { Vec::new() })
    }
    /// Copies a file from the agent's computer into storage and shows it in
    /// its chat. `Err` carries a message for the agent.
    fn share_file(&self, agent: AgentId, path: String) -> BoxFuture<Result<SharedFile, String>> {
        let _ = (agent, path);
        Box::pin(async { Err("sharing files is not supported here".to_owned()) })
    }
    /// Calls a connector tool as `agent`. `Err` carries a message for the agent.
    fn call_connector_tool(
        &self,
        agent: AgentId,
        name: String,
        arguments: Option<rmcp::model::JsonObject>,
    ) -> BoxFuture<Result<rmcp::model::CallToolResult, String>> {
        let _ = (agent, arguments);
        Box::pin(async move { Err(format!("unknown tool {name}")) })
    }
    /// A tool recorded a chat event (e.g. a question card) for the UI.
    fn event_recorded(&self, event: &StoredEvent) {
        let _ = event;
    }
    /// The turn `agent` is running, if any.
    fn current_turn(&self, agent: AgentId) -> Option<String> {
        let _ = agent;
        None
    }
    /// Asks the operator to approve a new agent and creates it if they do; resolves on the
    /// answer or the timeout. `Err` carries a message for the agent.
    fn request_agent(
        &self,
        requester: AgentId,
        request: AgentRequest,
    ) -> BoxFuture<Result<AgentRequestOutcome, String>> {
        let _ = (requester, request);
        Box::pin(async { Err("creating agents is not supported here".to_owned()) })
    }
    /// Shows the operator a card to add a connector, with a hidden token field when it takes
    /// one; returns the card's event id at once. `Err` carries a message for the agent.
    fn request_connector(
        &self,
        requester: AgentId,
        request: ConnectorRequest,
    ) -> BoxFuture<Result<i64, String>> {
        let _ = (requester, request);
        Box::pin(async { Err("adding connectors is not supported here".to_owned()) })
    }
    /// Creates a routine for `requester`, or shows the operator a card for it unless Auto mode
    /// is on. `Err` carries a message for the agent.
    fn request_routine(
        &self,
        requester: AgentId,
        request: RoutineRequest,
    ) -> BoxFuture<Result<RoutineRequestOutcome, String>> {
        let _ = (requester, request);
        Box::pin(async { Err("routines are not supported here".to_owned()) })
    }
    /// Changes the tools of an agent `requester` created: at once when that narrows them,
    /// else after the operator approves a card unless Auto mode is on. `Err` carries a message
    /// for the agent.
    fn set_agent_tools(
        &self,
        requester: AgentId,
        request: AgentToolsRequest,
    ) -> BoxFuture<Result<AgentToolsOutcome, String>> {
        let _ = (requester, request);
        Box::pin(async { Err("changing tools is not supported here".to_owned()) })
    }
    /// `agent` hangs up the voice call when its turn ends. `Ok(true)`: it was the host, so the
    /// call ends for everyone. `Err` carries a message for the agent.
    fn hang_up(&self, agent: AgentId) -> BoxFuture<Result<bool, String>> {
        let _ = agent;
        Box::pin(async { Err("voice calls are not supported here".to_owned()) })
    }
    /// Whether `agent` is in the operator's voice call.
    fn in_call(&self, agent: AgentId) -> BoxFuture<bool> {
        let _ = agent;
        Box::pin(async { false })
    }
    /// Brings `guest` into the operator's voice call that `host` is in
    /// and sends it `topic`. `Err` carries a message for the agent.
    fn invite_to_call(
        &self,
        host: AgentId,
        guest: AgentId,
        topic: String,
    ) -> BoxFuture<Result<(), String>> {
        let _ = (host, guest, topic);
        Box::pin(async { Err("voice calls are not supported here".to_owned()) })
    }
}

/// Shared state for the MCP endpoint.
#[derive(Clone)]
pub struct McpState {
    pub store: Store,
    pub notifier: Arc<dyn MailboxNotifier>,
}

impl std::fmt::Debug for McpState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpState").finish_non_exhaustive()
    }
}
