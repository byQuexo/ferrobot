//! ferrobot's own tools, exposed to agents as an MCP server (Streamable HTTP).
//!
//! Every request must carry `Authorization: Bearer <runner token>`. The
//! [`router`] middleware resolves the token to an agent and stores it as
//! [`Caller`] in the request extensions; tools read it from there, so an
//! agent can only ever act as itself.

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
    /// Asks the operator to approve a new agent and creates it if they do.
    /// Resolves once the operator answered or the request timed out. `Err`
    /// carries a message for the agent (invalid name, limit reached, ...).
    fn request_agent(
        &self,
        requester: AgentId,
        request: AgentRequest,
    ) -> BoxFuture<Result<AgentRequestOutcome, String>> {
        let _ = (requester, request);
        Box::pin(async { Err("creating agents is not supported here".to_owned()) })
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
