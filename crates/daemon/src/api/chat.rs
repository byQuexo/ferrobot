//! Mailbox, events and turn control: everything the chat view needs.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use ferrobot_core::question::{Approval, QuestionAnswer, QuestionAnswered, QuestionEvent};
use ferrobot_core::{MailMessage, Sender};
use ferrobot_store::StoredEvent;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::{ApiResult, MAX_TEXT_BYTES, parse_id};
use crate::error::{ApiError, ErrorBody};
use crate::views::AgentView;
use crate::{AppState, UiEvent};

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct PostMessage {
    /// 1..32768 bytes after trimming. `@name` mentions of existing agents are
    /// stored as `<@agent-id>` tokens, and the limit counts them as stored.
    body: String,
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
    let msg = state
        .store
        .enqueue_message(Sender::User, id, body, None)
        .await?;
    state.message_arrived(id);
    state.publish(UiEvent::Message {
        message: msg.clone(),
    });
    Ok((StatusCode::ACCEPTED, Json(msg)))
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
    answers: Vec<QuestionAnswer>,
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
    if let Some(approval @ Approval::AddConnector { .. }) = &question.approval {
        let answered = crate::api::connectors::answer_card(
            &state,
            id,
            qid,
            &question.card,
            approval,
            req.answers,
        )
        .await?;
        return Ok((StatusCode::ACCEPTED, Json(answered)));
    }
    if let Some(approval @ Approval::CreateRoutine { .. }) = &question.approval {
        let answered =
            crate::api::routines::answer_card(&state, id, qid, approval, req.answers).await?;
        return Ok((StatusCode::ACCEPTED, Json(answered)));
    }
    if let Some(approval @ Approval::SetAgentTools { .. }) = &question.approval {
        let answered =
            crate::agents::answer_tools_card(&state, id, qid, approval, req.answers).await?;
        return Ok((StatusCode::ACCEPTED, Json(answered)));
    }
    if let Some(approval @ Approval::UseWithoutTools { .. }) = &question.approval {
        let answered = crate::no_tools::answer_card(&state, id, qid, approval, req.answers).await?;
        return Ok((StatusCode::ACCEPTED, Json(answered)));
    }
    if question.approval.is_some() {
        let answered = QuestionAnswered {
            question: qid,
            answers: req.answers,
            message_id: None,
        };
        let waiting = crate::agents::decide(&state, qid, &answered.answers);
        let payload =
            serde_json::to_value(&answered).map_err(|e| ApiError::Internal(e.to_string()))?;
        state.record(id, None, "question_answered", payload).await;
        if !waiting {
            return Err(ApiError::Conflict(format!(
                "approval request {qid} is no longer pending"
            )));
        }
        return Ok((StatusCode::ACCEPTED, Json(answered)));
    }
    let body = question.card.answer_text(&req.answers);
    let msg = state
        .store
        .enqueue_message(Sender::User, id, body, None)
        .await?;
    let answered = QuestionAnswered {
        question: qid,
        answers: req.answers,
        message_id: Some(msg.id),
    };
    let payload = serde_json::to_value(&answered).map_err(|e| ApiError::Internal(e.to_string()))?;
    state.record(id, None, "question_answered", payload).await;
    state.message_arrived(id);
    state.publish(UiEvent::Message { message: msg });
    Ok((StatusCode::ACCEPTED, Json(answered)))
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Interrupted {
    /// The interrupted turn, or null if the agent was idle.
    turn_id: Option<String>,
}

/// Aborts the agent's running turn, if any. Interrupted messages are not retried.
#[utoipa::path(post, path = "/v1/agents/{id}/interrupt", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = Interrupted),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn interrupt(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Interrupted> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    Ok(Json(Interrupted {
        turn_id: state.hub.interrupt(id),
    }))
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
