use ferrobot_core::question::{QUESTION_CARD_VERSION, QuestionCard, QuestionEvent};
use ferrobot_core::{AgentId, AgentName, AvatarCharacter, MailMessage, Sender, mention};
use ferrobot_store::StoreError;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AgentRequest, AgentRequestOutcome, Caller, MAX_MEMORY_KEY_BYTES, MAX_MESSAGE_BYTES, McpState,
};

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
    /// Stable id; use it in `send_message.to`. Names can change.
    pub id: String,
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
    /// Recipient agent id (from `list_agents`). The agent's current name,
    /// e.g. "bob", also works.
    pub to: String,
    /// Message text.
    pub body: String,
    /// Optional id of the message this answers (one you received or sent);
    /// keeps the conversation threaded.
    pub reply_to: Option<i64>,
    /// `true`: urgent, wake the recipient now and handle this before its other
    /// queued messages. `false`: not urgent, don't wake it; it gets this on its
    /// next turn. Omit for normal delivery (queued, wakes the recipient).
    pub priority: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SendMessageResult {
    pub message_id: i64,
    /// Recipient's current name.
    pub queued_for: String,
    /// Recipient's id.
    pub agent_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateAgentArgs {
    /// Name of the new agent: 1-32 chars, `a-z0-9-`, starts with a letter.
    pub name: String,
    /// One line describing its role, shown to the operator and in agent lists.
    pub description: String,
    /// System instructions for the new agent's model. Optional.
    #[serde(default)]
    pub instructions: String,
    /// Optional character avatar look; omitted: derived from the new id.
    pub character: Option<AvatarCharacter>,
    /// The task to start on, with everything the new agent needs to know.
    /// Sent as your first message to it, so it starts right away and
    /// reports back to you. Omitted: it stays idle until you message it.
    pub task: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CreateAgentResult {
    /// `created` or `declined`.
    pub status: String,
    /// Id of the new agent (when created); use it in `send_message.to`.
    pub agent_id: Option<String>,
    pub name: String,
    /// What happened, in words.
    pub note: String,
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
pub struct AskUserResult {
    /// Id of the card in your chat.
    pub question_id: i64,
    pub note: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Ack {
    pub ok: bool,
}

fn check_body(body: &str) -> Result<&str, ErrorData> {
    let body = body.trim();
    if body.is_empty() {
        return Err(ErrorData::invalid_params("body must not be empty", None));
    }
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(ErrorData::invalid_params(
            format!("body exceeds {MAX_MESSAGE_BYTES} bytes"),
            None,
        ));
    }
    Ok(body)
}

impl FerrobotTools {
    /// Queues mail from agent `from`, with `@name` mentions stored as tokens.
    async fn enqueue(
        &self,
        from: AgentId,
        to: AgentId,
        body: &str,
        reply_to: Option<i64>,
        priority: Option<bool>,
    ) -> Result<MailMessage, ErrorData> {
        let store = &self.state.store;
        let agents = store.list_agents().await.map_err(store_err)?;
        let body = mention::normalize(body, |name| {
            agents
                .iter()
                .find(|a| a.name.as_str() == name)
                .map(|a| a.id)
        });
        let msg = store
            .enqueue_message_with_priority(Sender::Agent(from), to, body, reply_to, priority)
            .await
            .map_err(store_err)?;
        self.state.notifier.message_enqueued(&msg);
        Ok(msg)
    }
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
                    id: a.id.to_string(),
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
        let body = check_body(&args.body)?;
        let to = args.to.trim().trim_start_matches('@');
        let store = &self.state.store;
        // Ids are stable; names are unique, so a name lookup is unambiguous.
        let recipient = if let Ok(id) = to.parse::<AgentId>() {
            store.get_agent(id).await
        } else {
            let name =
                AgentName::parse(to).map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            store.get_agent_by_name(name).await
        }
        .map_err(store_err)?;
        if recipient.id == me.id {
            return Err(ErrorData::invalid_params(
                "cannot send a message to yourself",
                None,
            ));
        }
        if let Some(reply_to) = args.reply_to {
            let original = store.get_message(reply_to).await.map_err(store_err)?;
            if original.recipient != me.id && original.sender != Sender::Agent(me.id) {
                return Err(ErrorData::invalid_params(
                    "reply_to must be a message you received or sent",
                    None,
                ));
            }
        }
        let msg = self
            .enqueue(me.id, recipient.id, body, args.reply_to, args.priority)
            .await?;
        Ok(Json(SendMessageResult {
            message_id: msg.id,
            queued_for: recipient.name.to_string(),
            agent_id: recipient.id.to_string(),
        }))
    }

    #[tool(
        description = "Show your operator a question card in your chat: one or more questions, \
        each single choice, multiple choice or free text (choices can allow a custom answer). \
        Does not wait: the answers arrive later as one user message, so finish your turn after asking."
    )]
    async fn ask_user(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(mut card): Parameters<QuestionCard>,
    ) -> ToolResult<AskUserResult> {
        let me = caller(&parts)?.0.id;
        card.normalize()
            .map_err(|e| ErrorData::invalid_params(e, None))?;
        let payload = serde_json::to_value(QuestionEvent {
            version: QUESTION_CARD_VERSION,
            card,
            approval: None,
        })
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let event = self
            .state
            .store
            .append_event(me, None, "question".into(), payload)
            .await
            .map_err(store_err)?;
        self.state.notifier.event_recorded(&event);
        Ok(Json(AskUserResult {
            question_id: event.id,
            note: "Shown to the user. Their answers arrive later as a new message; finish your turn now."
                .into(),
        }))
    }

    #[tool(
        description = "Create a new agent. The operator first sees an approval card with its name and role; this call waits for their decision. Returns `created` with the new agent's id, or `declined` (operator said no or did not answer in time). Pass `task` to brief it: it is sent as your first message, the agent starts right away and reports back to you. Fails if the name is invalid or taken, or the agent limit is reached."
    )]
    async fn create_agent(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(args): Parameters<CreateAgentArgs>,
    ) -> ToolResult<CreateAgentResult> {
        let me = caller(&parts)?.0.id;
        let task = match args.task.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => Some(check_body(t)?.to_owned()),
            _ => None,
        };
        let request = AgentRequest {
            name: args.name.trim().to_owned(),
            description: args.description,
            instructions: args.instructions,
            character: args.character,
        };
        let outcome = self
            .state
            .notifier
            .request_agent(me, request.clone())
            .await
            .map_err(|e| ErrorData::invalid_params(e, None))?;
        Ok(Json(match outcome {
            AgentRequestOutcome::Created { id, name } => {
                let note = match &task {
                    Some(task) => {
                        let msg = self.enqueue(me, id, task, None, None).await?;
                        format!(
                            "Created \"{name}\" and sent it the task as message #{}. \
                             It reports back to you.",
                            msg.id
                        )
                    }
                    None => format!(
                        "Created \"{name}\". It has no task and stays idle until you \
                         message it with send_message."
                    ),
                };
                CreateAgentResult {
                    status: "created".into(),
                    agent_id: Some(id.to_string()),
                    note,
                    name,
                }
            }
            AgentRequestOutcome::Declined { reason } => CreateAgentResult {
                status: "declined".into(),
                agent_id: None,
                name: request.name,
                note: format!("Not created: {reason}"),
            },
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
                    body: mention::render(&m.body, |id| {
                        agents
                            .iter()
                            .find(|a| a.id == id)
                            .map(|a| a.name.to_string())
                    }),
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
    instructions = "Mailbox, memory, question-card and agent-creation tools for ferrobot agents. Messages and answers are asynchronous."
)]
impl ServerHandler for FerrobotTools {}
