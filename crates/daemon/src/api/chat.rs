//! Mailbox, events and turn control: everything the chat view needs.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use ferrobot_core::{MailMessage, Sender};
use ferrobot_store::StoredEvent;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::{ApiResult, MAX_TEXT_BYTES, parse_id};
use crate::error::{ApiError, ErrorBody};
use crate::{AppState, UiEvent};

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct PostMessage {
    /// 1..32768 bytes after trimming.
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
    let body = req.body.trim();
    if body.is_empty() || body.len() > MAX_TEXT_BYTES {
        return Err(ApiError::BadRequest("body must be 1..32768 bytes".into()));
    }
    let msg = state
        .store
        .enqueue_message(Sender::User, id, body.to_owned(), None)
        .await?;
    state.message_arrived(id);
    state.publish(UiEvent::Message {
        message: msg.clone(),
    });
    Ok((StatusCode::ACCEPTED, Json(msg)))
}

/// The agent's conversation: messages to and from it, oldest first (last 200).
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
