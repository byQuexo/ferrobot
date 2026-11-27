//! Mailbox, events and turn control: everything the chat view needs.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use ferrobot_core::question::{Approval, QuestionAnswer, QuestionAnswered, QuestionEvent};
use ferrobot_core::{MailMessage, Sender};
use ferrobot_store::StoredEvent;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::{ApiResult, MAX_TEXT_BYTES, parse_id};
use crate::error::{ApiError, ErrorBody};
use crate::views::AgentView;
use crate::{AppState, UiEvent};

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/agents/{id}/messages",
            get(list_messages).post(post_message),
        )
        .route("/v1/agents/{id}/events/{event_id}/reaction", post(react))
        .route("/v1/agents/{id}/events", get(list_events))
        .route("/v1/agents/{id}/read", post(mark_read))
        .route("/v1/conversations", get(list_conversations))
        .route("/v1/conversations/{a}/{b}", get(conversation_thread))
        .route("/v1/agents/{id}/interrupt", post(interrupt))
        .route("/v1/agents/{id}/retry", post(retry))
        .route("/v1/agents/{id}/new-session", post(new_session))
        .route("/v1/agents/{id}/turns/{turn_id}/trace", get(turn_trace))
        .route("/v1/agents/{id}/compact", post(compact))
        .route(
            "/v1/agents/{id}/questions/{qid}/answer",
            post(answer_question),
        )
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct PostMessage {
    /// 1..32768 bytes after trimming. `@name` mentions of existing agents are
    /// stored as `<@agent-id>` tokens, and the limit counts them as stored.
    body: String,
    /// Send now: if the agent is in a turn that cannot take the message (its runner does not
    /// steer), that turn stops so the message goes next. Otherwise it waits for the turn to end.
    #[serde(default)]
    now: bool,
}

/// Sends a message from the operator (`user`) to an agent's mailbox. Wakes
/// the agent if it sleeps.
#[utoipa::path(post, path = "/v1/agents/{id}/messages", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    request_body = PostMessage,
    responses(
        (status = 202, description = "Queued for delivery", body = MailMessage),
        (status = 400, body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn post_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PostMessage>,
) -> Result<(StatusCode, Json<MailMessage>), ApiError> {
    let id = parse_id(&id)?;
    let body = state.normalize_mentions(req.body.trim()).await;
    if body.is_empty() || body.len() > MAX_TEXT_BYTES {
        return Err(ApiError::BadRequest("body must be 1..32768 bytes".into()));
    }
    let msg = crate::agents::tell(&state, id, body).await?;
    if req.now && state.hub.turn_holds_messages(id) {
        state.hub.interrupt(id);
    }
    Ok((StatusCode::ACCEPTED, Json(msg)))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct ReactionRequest {
    /// One emoji, e.g. 👍. A new reaction replaces your earlier one on that message.
    emoji: String,
}

/// How much of the agent's message its reaction note quotes.
const REACTION_QUOTE_CHARS: usize = 200;

/// Reacts to one of the agent's messages (an `assistant_text` event) with one emoji. The agent
/// hears of it in a short `[ferrobot]` note from the operator, which starts a turn; the chat shows
/// the emoji on its message (a `user_reaction` event naming the note as `note_id`), not the note.
#[utoipa::path(post, path = "/v1/agents/{id}/events/{event_id}/reaction", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)"),
        ("event_id" = i64, Path, description = "Id of the agent's `assistant_text` event")),
    request_body = ReactionRequest,
    responses((status = 204, description = "Recorded, and the agent was told"),
        (status = 400, description = "Not one emoji", body = ErrorBody),
        (status = 404, description = "Unknown agent or message", body = ErrorBody)))]
pub(crate) async fn react(
    State(state): State<AppState>,
    Path((id, event_id)): Path<(String, i64)>,
    Json(req): Json<ReactionRequest>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let emoji = req.emoji.trim();
    if !ferrobot_core::is_reaction(emoji) {
        return Err(ApiError::BadRequest(
            "emoji must be one emoji, like 👍".into(),
        ));
    }
    let event = state.store.get_event(id, event_id).await?;
    if event.kind != "assistant_text" {
        return Err(ApiError::NotFound("message not found".into()));
    }
    let text = event.payload["text"].as_str().unwrap_or_default();
    let mut quote: String = text.chars().take(REACTION_QUOTE_CHARS).collect();
    if quote.len() < text.len() {
        quote.push('…');
    }
    let body = format!(
        "[ferrobot] Your operator reacted {emoji} to your message: \"{quote}\". Act on it only \
         if it asks for something, like a fix or an answer; otherwise end your turn without a reply."
    );
    let note = state
        .store
        .enqueue_message(Sender::User, id, body, None)
        .await?;
    let payload = serde_json::json!({ "event_id": event_id, "emoji": emoji, "note_id": note.id });
    // Before the note is published, so the chat hides it from the start.
    state.record(id, None, "user_reaction", payload).await;
    state.message_arrived(id);
    state.publish(UiEvent::Message { message: note });
    Ok(StatusCode::NO_CONTENT)
}

/// The agent's conversation: messages to and from it, oldest first (last 200). Call room turns
/// (`room_id` set) are left out; they are in the room.
#[utoipa::path(get, path = "/v1/agents/{id}/messages", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = Vec<MailMessage>),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn list_messages(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Vec<MailMessage>> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    Ok(Json(state.store.conversation(id, 200).await?))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct EventsQuery {
    /// Return events with an id greater than this (default 0).
    #[serde(default)]
    after: i64,
}

/// Turn events (incoming messages, text, tool calls and results, lifecycle),
/// oldest first, up to 500 per call; page with `after`.
#[utoipa::path(get, path = "/v1/agents/{id}/events", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)"), EventsQuery),
    responses((status = 200, body = Vec<StoredEvent>),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn list_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Vec<StoredEvent>> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    Ok(Json(state.store.events_after(id, q.after, 500).await?))
}

/// Messages two agents exchanged.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Conversation {
    /// The two agents, ordered by id.
    a: ferrobot_core::AgentId,
    b: ferrobot_core::AgentId,
    /// Messages in both directions.
    messages: u32,
    /// The most recent one.
    last: MailMessage,
}

/// Agent-to-agent conversations (one per pair of agents), most recent first; call room turns
/// are left out.
#[utoipa::path(get, path = "/v1/conversations", tag = "chat",
    responses((status = 200, body = Vec<Conversation>)))]
pub(crate) async fn list_conversations(
    State(state): State<AppState>,
) -> ApiResult<Vec<Conversation>> {
    let pairs = state.store.agent_pairs().await?;
    Ok(Json(
        pairs
            .into_iter()
            .map(|p| Conversation {
                a: p.a,
                b: p.b,
                messages: p.messages,
                last: p.last,
            })
            .collect(),
    ))
}

/// Read-only thread between two agents: messages in both directions, oldest
/// first (last 200), without call room turns. The order of `a` and `b` does not matter.
#[utoipa::path(get, path = "/v1/conversations/{a}/{b}", tag = "chat",
    params(("a" = String, Path, description = "Agent id (UUID)"),
        ("b" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = Vec<MailMessage>),
        (status = 404, description = "Malformed agent id", body = ErrorBody)))]
pub(crate) async fn conversation_thread(
    State(state): State<AppState>,
    Path((a, b)): Path<(String, String)>,
) -> ApiResult<Vec<MailMessage>> {
    let (a, b) = (parse_id(&a)?, parse_id(&b)?);
    Ok(Json(state.store.pair_thread(a, b, 200).await?))
}

/// Marks the agent's chat as read (resets `unread`).
#[utoipa::path(post, path = "/v1/agents/{id}/read", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 204, description = "Marked as read"),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn mark_read(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    state.store.mark_read(id).await?;
    state.publish_agent(id).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct AnswerQuestion {
    /// One answer per question of the card.
    pub(crate) answers: Vec<QuestionAnswer>,
}

/// Answers a question card: the agent gets the answers as one user message and the card locks.
/// An approval card's decision goes to the waiting tool call instead (`409` once it timed out).
#[utoipa::path(post, path = "/v1/agents/{id}/questions/{qid}/answer", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)"),
        ("qid" = i64, Path, description = "Id of the `question` event")),
    request_body = AnswerQuestion,
    responses(
        (status = 202, description = "Answer queued for the agent", body = QuestionAnswered),
        (status = 400, description = "Answers do not fit the card", body = ErrorBody),
        (status = 404, description = "Unknown agent or question", body = ErrorBody),
        (status = 409, description = "Already answered", body = ErrorBody)))]
pub(crate) async fn answer_question(
    State(state): State<AppState>,
    Path((id, qid)): Path<(String, i64)>,
    Json(req): Json<AnswerQuestion>,
) -> Result<(StatusCode, Json<QuestionAnswered>), ApiError> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    let event = state.store.get_event(id, qid).await?;
    if event.kind != "question" {
        return Err(ApiError::NotFound(format!("question {qid} not found")));
    }
    let question: QuestionEvent = serde_json::from_value(event.payload)
        .map_err(|e| ApiError::Internal(format!("stored question {qid}: {e}")))?;
    question
        .card
        .check_answers(&req.answers)
        .map_err(ApiError::BadRequest)?;
    let _guard = state.answers.lock().await;
    if state.store.question_answered(id, qid).await? {
        return Err(ApiError::Conflict(format!(
            "question {qid} is already answered"
        )));
    }
    let answered = match &question.approval {
        Some(approval @ Approval::AddConnector { .. }) => {
            let card = &question.card;
            crate::api::connectors::answer_card(&state, id, qid, card, approval, req.answers)
                .await?
        }
        Some(approval @ Approval::AddCli { .. }) => {
            crate::api::clis::answer_card(&state, id, qid, approval, req.answers).await?
        }
        Some(approval @ Approval::AddCommand { .. }) => {
            crate::api::commands::answer_card(&state, id, qid, approval, req.answers).await?
        }
        Some(approval @ Approval::CreateRoutine { .. }) => {
            crate::api::routines::answer_card(&state, id, qid, approval, req.answers).await?
        }
        Some(approval @ Approval::SetAgentTools { .. }) => {
            crate::agents::answer_tools_card(&state, id, qid, approval, req.answers).await?
        }
        Some(approval @ Approval::UseWithoutTools { .. }) => {
            crate::no_tools::answer_card(&state, id, qid, approval, req.answers).await?
        }
        Some(_) => {
            let answered = QuestionAnswered {
                question: qid,
                answers: req.answers,
                message_id: None,
                timed_out: false,
            };
            let waiting = crate::agents::decide(&state, qid, &answered.answers);
            let answered = crate::agents::record_answer(&state, id, answered).await?;
            if !waiting {
                return Err(ApiError::Conflict(format!(
                    "approval request {qid} is no longer pending"
                )));
            }
            answered
        }
        None => {
            let body = question.card.answer_text(&req.answers);
            let msg = state
                .store
                .enqueue_message(Sender::User, id, body, None)
                .await?;
            let answered = QuestionAnswered {
                question: qid,
                answers: req.answers,
                message_id: Some(msg.id),
                timed_out: false,
            };
            let answered = crate::agents::record_answer(&state, id, answered).await?;
            state.message_arrived(id);
            state.publish(UiEvent::Message { message: msg });
            answered
        }
    };
    Ok((StatusCode::ACCEPTED, Json(answered)))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct InterruptQuery {
    /// Also stop every agent working on a message from this one (and from those, and so on), and
    /// drop the agent messages queued to or from any of them.
    #[serde(default)]
    chain: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Interrupted {
    /// The interrupted turn, or null if the agent was idle.
    turn_id: Option<String>,
    /// With `chain`: every agent whose work was stopped, this one first.
    #[schema(value_type = Vec<String>)]
    agents: Vec<ferrobot_core::AgentId>,
    /// With `chain`: how many queued agent messages were dropped.
    dropped: usize,
}

/// Aborts the agent's running turn, if any; with `chain`, the work it set off as well and its
/// task with a goal (Stop). Interrupted messages are not retried.
#[utoipa::path(post, path = "/v1/agents/{id}/interrupt", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)"), InterruptQuery),
    responses((status = 200, body = Interrupted),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn interrupt(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<InterruptQuery>,
) -> ApiResult<Interrupted> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    let turn_id = state.hub.current_turn(id);
    let (agents, dropped) = if q.chain {
        let stopped = "stopped by the operator";
        crate::goal::end(&state, id, crate::goal::End::Stopped, stopped).await;
        crate::stop::stop_chain(&state, id).await?
    } else {
        state.hub.interrupt(id);
        (Vec::new(), 0)
    };
    Ok(Json(Interrupted {
        turn_id,
        agents,
        dropped,
    }))
}

/// Compacts the agent's conversation: its model keeps a summary instead of the whole history,
/// so its context shrinks. Claude Code agents only (their own `/compact`, run by the daemon).
#[utoipa::path(post, path = "/v1/agents/{id}/compact", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 204, description = "Compacted"),
        (status = 400, description = "Not a Claude Code agent", body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody),
        (status = 409, description = "Nothing to compact yet, or the agent is in a turn", body = ErrorBody),
        (status = 502, description = "Claude Code failed to compact", body = ErrorBody)))]
pub(crate) async fn compact(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let agent = state.store.get_agent(parse_id(&id)?).await?;
    if !crate::agents::on_claude_code(&state, &agent).await {
        return Err(ApiError::BadRequest(
            "compacting works for Claude Code agents for now".into(),
        ));
    }
    match crate::claude_code::compact(&state, agent.id).await {
        Ok(_) => {
            state
                .record(agent.id, None, "compacted", serde_json::json!({}))
                .await;
            Ok(StatusCode::NO_CONTENT)
        }
        Err(crate::claude_code::CompactError::NotNow(why)) => Err(ApiError::Conflict(why.into())),
        Err(crate::claude_code::CompactError::Failed(why)) => Err(ApiError::Upstream(why)),
    }
}

/// One of the agent's turns as a timeline: its model calls and tool calls with their times, the
/// cards it waited on and the messages it handed to other agents.
#[utoipa::path(get, path = "/v1/agents/{id}/turns/{turn_id}/trace", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)"),
        ("turn_id" = String, Path, description = "Turn id, as on the turn's events")),
    responses((status = 200, body = crate::trace::Trace),
        (status = 404, description = "Unknown agent or turn", body = ErrorBody)))]
pub(crate) async fn turn_trace(
    State(state): State<AppState>,
    Path((id, turn)): Path<(String, String)>,
) -> ApiResult<crate::trace::Trace> {
    let agent = state.store.get_agent(parse_id(&id)?).await?;
    let (trace, _) = crate::trace::for_turn(&state, &agent, &turn).await?;
    trace
        .map(Json)
        .ok_or_else(|| ApiError::NotFound("turn not found".into()))
}

/// Starts a new chat: the agent's next turn begins a fresh model session without the earlier
/// messages. The chat history stays visible.
#[utoipa::path(post, path = "/v1/agents/{id}/new-session", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 204, description = "The agent's next turn starts fresh"),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn new_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    // The event is the pending reset: it survives restarts and a sleeping runner.
    let event = state
        .store
        .append_event(id, None, "new_session".into(), serde_json::json!({}))
        .await?;
    state.publish(UiEvent::Event { event });
    Ok(StatusCode::NO_CONTENT)
}

/// Clears the agent's credential or tools problem so its waiting messages run; if it still
/// stands, the next model call sets it again.
#[utoipa::path(post, path = "/v1/agents/{id}/retry", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = AgentView),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn retry(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<AgentView> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    crate::credentials::clear(&state, id).await;
    Ok(Json(crate::views::agent_view(&state, id).await?))
}
