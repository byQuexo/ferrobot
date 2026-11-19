//! ferrobot's own tools, exposed to agents as an MCP server (Streamable HTTP).
//!
//! Every request must carry `Authorization: Bearer <runner token>`. The
//! [`router`] middleware resolves the token to an agent and stores it as
//! [`Caller`] in the request extensions; tools read it from there, so an
//! agent can only ever act as itself.

mod auth;
mod tools;

use std::sync::Arc;

use ferrobot_core::AgentId;
use ferrobot_store::Store;

pub use auth::{Caller, router};
pub use tools::FerrobotTools;

/// MCP server name as seen by clients. Claude exposes tools as `mcp__<name>__<tool>`.
pub const SERVER_NAME: &str = "ferrobot";

/// Upper bound for one message body (bytes).
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024;

/// Called after a tool enqueued a message, so the daemon can dispatch it.
pub trait MailboxNotifier: Send + Sync + 'static {
    fn message_enqueued(&self, recipient: AgentId, message_id: i64);
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
