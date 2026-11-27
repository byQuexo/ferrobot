//! An agent's task with a goal (`/v1/agents/{id}/goal`), the chat's `/task`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use utoipa::ToSchema;

use super::{ApiResult, MAX_TEXT_BYTES, parse_id};
use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::goal::{self, DEFAULT_MAX_CONTINUATIONS, End, MAX_CONTINUATIONS, TaskGoal};

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct SetGoal {
    /// What the agent should get done; 1..32768 bytes after trimming.
    goal: String,
    /// How often the agent is sent back to work before it gives up; 1-100, default 20.
    #[serde(default)]
    max_continuations: Option<u32>,
}

/// The agent's task with a goal.
#[utoipa::path(get, path = "/v1/agents/{id}/goal", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = TaskGoal),
        (status = 404, description = "Unknown agent, or it has no task", body = ErrorBody)))]
pub(crate) async fn get_goal(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<TaskGoal> {
    let id = parse_id(&id)?;
    goal::get(&state, id)
        .await
        .map(Json)
        .ok_or_else(|| ApiError::NotFound("the agent has no task".into()))
}

/// Gives the agent a task with a goal, replacing its current one. The goal goes out as the
/// operator's message. After each turn on one of the operator's messages the agent is sent back
/// to work until it calls `finish_task` and has reported; then, for Claude Code, Anthropic and
/// OpenAI-compatible providers, a model checks its work. A stopped, interrupted or failed turn
/// ends the task, and so does reaching `max_continuations`.
#[utoipa::path(put, path = "/v1/agents/{id}/goal", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    request_body = SetGoal,
    responses((status = 200, body = TaskGoal),
        (status = 400, description = "An empty or too long goal, or a bad limit", body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn put_goal(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetGoal>,
) -> ApiResult<TaskGoal> {
    let agent = state.store.get_agent(parse_id(&id)?).await?;
    let text = req.goal.trim();
    if text.is_empty() || text.len() > MAX_TEXT_BYTES {
        return Err(ApiError::BadRequest("goal must be 1..32768 bytes".into()));
    }
    let max = req.max_continuations.unwrap_or(DEFAULT_MAX_CONTINUATIONS);
    if !(1..=MAX_CONTINUATIONS).contains(&max) {
        return Err(ApiError::BadRequest(format!(
            "max_continuations must be 1-{MAX_CONTINUATIONS}"
        )));
    }
    Ok(Json(
        goal::set(&state, agent.id, text.to_owned(), max).await?,
    ))
}

/// Clears the agent's task without stopping its turn; a waiting continuation is dropped.
#[utoipa::path(delete, path = "/v1/agents/{id}/goal", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 204, description = "Cleared, or there was none"),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn delete_goal(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let agent = state.store.get_agent(parse_id(&id)?).await?;
    goal::end(&state, agent.id, End::Stopped, "cleared by the operator").await;
    Ok(StatusCode::NO_CONTENT)
}
