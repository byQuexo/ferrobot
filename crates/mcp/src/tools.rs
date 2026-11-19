use ferrobot_core::{AgentName, Sender};
use ferrobot_store::StoreError;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Caller, MAX_MEMORY_KEY_BYTES, MAX_MESSAGE_BYTES, McpState};

type ToolResult<T> = Result<Json<T>, ErrorData>;

/// The tool set. One instance per MCP session.
#[derive(Clone)]
pub struct FerrobotTools {
    state: McpState,
}

impl std::fmt::Debug for FerrobotTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FerrobotTools").finish_non_exhaustive()
    }
}

fn caller(parts: &http::request::Parts) -> Result<&Caller, ErrorData> {
    parts
        .extensions
        .get::<Caller>()
        .ok_or_else(|| ErrorData::invalid_request("unauthenticated", None))
}

fn store_err(e: StoreError) -> ErrorData {
    match e {
        StoreError::NotFound { .. } | StoreError::Conflict { .. } | StoreError::Core(_) => {
            ErrorData::invalid_params(e.to_string(), None)
        }
        other => {
            tracing::error!(error = %other, "store failure in MCP tool");
            ErrorData::internal_error("internal storage error", None)
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AgentInfo {
    pub name: String,
    pub status: String,
    /// One-line description.
    pub description: String,
    pub instructions: String,
    /// True for the calling agent itself.
    pub is_you: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AgentList {
    pub agents: Vec<AgentInfo>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendMessageArgs {
    /// Recipient agent name, e.g. "bob".
    pub to: String,
    /// Message text.
    pub body: String,
    /// Optional id of the message this answers.
    pub reply_to: Option<i64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SendMessageResult {
    pub message_id: i64,
    pub queued_for: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadMailboxArgs {
    /// How many recent messages to return (default 20, max 100).
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MailboxEntry {
    pub id: i64,
    pub from: String,
    pub body: String,
    pub reply_to: Option<i64>,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Mailbox {
    pub messages: Vec<MailboxEntry>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RememberArgs {
    /// Short identifier, e.g. "deploy-days". Re-using a key overwrites it.
    pub key: String,
    pub content: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecallArgs {
    /// Words to search for. Empty returns the most recent memories.
    pub query: Option<String>,
    /// Max results (default 10, max 50).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ForgetArgs {
    pub key: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MemoryItem {
    pub key: String,
    pub content: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Memories {
    pub memories: Vec<MemoryItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Ack {
    pub ok: bool,
}

#[tool_router]
impl FerrobotTools {
    pub fn new(state: McpState) -> Self {
        Self { state }
    }

    #[tool(description = "List all agents on this ferrobot host with their status and role.")]
    async fn list_agents(
        &self,
        Extension(parts): Extension<http::request::Parts>,
    ) -> ToolResult<AgentList> {
        let me = caller(&parts)?.0.id;
        let agents = self.state.store.list_agents().await.map_err(store_err)?;
        Ok(Json(AgentList {
            agents: agents
                .into_iter()
                .map(|a| AgentInfo {
                    is_you: a.id == me,
                    name: a.name.to_string(),
                    status: a.status.as_str().to_owned(),
                    description: a.description,
                    instructions: a.instructions,
                })
                .collect(),
        }))
    }

    #[tool(
        description = "Send an asynchronous message to another agent's mailbox. \
        The recipient handles it in its own turn; replies arrive later as new messages."
    )]
    async fn send_message(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(args): Parameters<SendMessageArgs>,
    ) -> ToolResult<SendMessageResult> {
        let me = &caller(&parts)?.0;
        let body = args.body.trim();
        if body.is_empty() {
            return Err(ErrorData::invalid_params("body must not be empty", None));
        }
        if body.len() > MAX_MESSAGE_BYTES {
            return Err(ErrorData::invalid_params(
                format!("body exceeds {MAX_MESSAGE_BYTES} bytes"),
                None,
            ));
        }
        let name = AgentName::parse(args.to.trim().trim_start_matches('@'))
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        if name == me.name {
            return Err(ErrorData::invalid_params(
                "cannot send a message to yourself",
                None,
            ));
        }
        let recipient = self
            .state
            .store
            .get_agent_by_name(name)
            .await
            .map_err(store_err)?;
        let msg = self
            .state
            .store
            .enqueue_message(
                Sender::Agent(me.id),
                recipient.id,
                body.to_owned(),
                args.reply_to,
            )
            .await
            .map_err(store_err)?;
        self.state.notifier.message_enqueued(recipient.id, msg.id);
        Ok(Json(SendMessageResult {
            message_id: msg.id,
            queued_for: recipient.name.to_string(),
        }))
    }

    #[tool(description = "Read the most recent messages addressed to you (newest last).")]
    async fn read_mailbox(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(args): Parameters<ReadMailboxArgs>,
    ) -> ToolResult<Mailbox> {
        let me = caller(&parts)?.0.id;
        let limit = args.limit.unwrap_or(20).clamp(1, 100);
        let store = &self.state.store;
        let messages = store.inbox(me, limit).await.map_err(store_err)?;
        let agents = store.list_agents().await.map_err(store_err)?;
        let name_of = |s: &Sender| match s {
            Sender::User => "user".to_owned(),
            Sender::Agent(id) => agents
                .iter()
                .find(|a| a.id == *id)
                .map_or_else(|| format!("agent:{id}"), |a| a.name.to_string()),
            Sender::Routine(id) => format!("routine:{id}"),
        };
        Ok(Json(Mailbox {
            messages: messages
                .into_iter()
                .map(|m| MailboxEntry {
                    id: m.id,
                    from: name_of(&m.sender),
                    body: m.body,
                    reply_to: m.reply_to,
                    status: m.status.as_str().to_owned(),
                    created_at: m.created_at.to_string(),
                })
                .collect(),
        }))
    }

    #[tool(description = "Store a long-term memory under a key (overwrites an existing key).")]
    async fn remember(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(args): Parameters<RememberArgs>,
    ) -> ToolResult<Ack> {
        let me = caller(&parts)?.0.id;
        let key = args.key.trim();
        if key.is_empty() || key.len() > MAX_MEMORY_KEY_BYTES {
            return Err(ErrorData::invalid_params(
                format!("key must be 1-{MAX_MEMORY_KEY_BYTES} bytes"),
                None,
            ));
        }
        if args.content.len() > MAX_MESSAGE_BYTES {
            return Err(ErrorData::invalid_params("content too large", None));
        }
        self.state
            .store
            .remember(me, key.to_owned(), args.content)
            .await
            .map_err(store_err)?;
        self.state.notifier.memory_changed(me);
        Ok(Json(Ack { ok: true }))
    }

    #[tool(description = "Search your long-term memories (full-text, best match first).")]
    async fn recall(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(args): Parameters<RecallArgs>,
    ) -> ToolResult<Memories> {
        let me = caller(&parts)?.0.id;
        let limit = args.limit.unwrap_or(10).clamp(1, 50);
        let hits = self
            .state
            .store
            .recall(me, args.query.unwrap_or_default(), limit)
            .await
            .map_err(store_err)?;
        Ok(Json(Memories {
            memories: hits
                .into_iter()
                .map(|m| MemoryItem {
                    key: m.key,
                    content: m.content,
                    updated_at: m.updated_at.to_string(),
                })
                .collect(),
        }))
    }

    #[tool(description = "Delete one of your memories by key.")]
    async fn forget(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(args): Parameters<ForgetArgs>,
    ) -> ToolResult<Ack> {
        let me = caller(&parts)?.0.id;
        let existed = self
            .state
            .store
            .forget(me, args.key)
            .await
            .map_err(store_err)?;
        if existed {
            self.state.notifier.memory_changed(me);
        }
        Ok(Json(Ack { ok: existed }))
    }
}

// The rmcp macro generates `async` trait methods without awaits.
#[allow(clippy::unused_async_trait_impl)]
#[tool_handler(
    name = "ferrobot",
    instructions = "Mailbox and memory tools for ferrobot agents. Messages are asynchronous."
)]
impl ServerHandler for FerrobotTools {}
