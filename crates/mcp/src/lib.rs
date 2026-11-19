//! ferrobot's own tools, exposed to agents as an MCP server (Streamable HTTP).
//!
//! Every request must carry `Authorization: Bearer <runner token>`. The
//! [`router`] middleware resolves the token to an agent and stores it as
//! [`Caller`] in the request extensions; tools read it from there, so an
//! agent can only ever act as itself.

mod auth;
mod tools;

use std::sync::Arc;

use ferrobot_core::{AgentId, MailMessage};
use ferrobot_store::{Store, StoredEvent};

pub use auth::{Caller, router};
pub use tools::FerrobotTools;

/// MCP server name as seen by clients. Claude exposes tools as `mcp__<name>__<tool>`.
pub const SERVER_NAME: &str = "ferrobot";

/// Upper bound for one message body (bytes).
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024;

/// Upper bound for a memory key (bytes).
pub const MAX_MEMORY_KEY_BYTES: usize = 128;

/// Hooks the daemon uses to react to tool calls.
pub trait MailboxNotifier: Send + Sync + 'static {
    /// A tool enqueued a message, so the daemon can dispatch it (and wake
    /// the recipient unless `message.priority == Some(false)`).
    fn message_enqueued(&self, message: &MailMessage);
    /// A tool changed the agent's memory.
    fn memory_changed(&self, agent: AgentId) {
        let _ = agent;
    }
    /// A tool recorded a chat event (e.g. a question card) for the UI.
    fn event_recorded(&self, event: &StoredEvent) {
        let _ = event;
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
