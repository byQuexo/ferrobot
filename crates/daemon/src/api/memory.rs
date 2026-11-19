//! Per-agent long-term memory (the same store the MCP tools use).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use ferrobot_mcp::MAX_MEMORY_KEY_BYTES;
use ferrobot_store::MemoryEntry;
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

use super::{ApiResult, check_len, parse_id};
use crate::error::{ApiError, ErrorBody};
use crate::{AppState, UiEvent};

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct MemoryQuery {
    /// Full-text search; empty lists the most recently updated entries.
    #[serde(default)]
    q: String,
    /// 1-200, default 100.
    limit: Option<u32>,
}

/// Lists or searches the agent's memories.
#[utoipa::path(get, path = "/v1/agents/{id}/memory", tag = "memory",
    params(("id" = String, Path, description = "Agent id (UUID)"), MemoryQuery),
    responses((status = 200, body = Vec<MemoryEntry>),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn list_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<MemoryQuery>,
) -> ApiResult<Vec<MemoryEntry>> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    let limit = q.limit.unwrap_or(100).clamp(1, 200);
    Ok(Json(state.store.recall(id, q.q, limit).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct PutMemory {
    /// 1-128 bytes; an existing key is overwritten.
    key: String,
    content: String,
}

/// Creates or replaces a memory.
#[utoipa::path(post, path = "/v1/agents/{id}/memory", tag = "memory",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    request_body = PutMemory,
    responses((status = 204, description = "Stored"),
        (status = 400, body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn put_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PutMemory>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let key = req.key.trim();
    if key.is_empty() || key.len() > MAX_MEMORY_KEY_BYTES {
        return Err(ApiError::BadRequest(format!(
            "key must be 1-{MAX_MEMORY_KEY_BYTES} bytes"
        )));
    }
    check_len("content", &req.content)?;
    state.store.get_agent(id).await?;
    state
        .store
        .remember(id, key.to_owned(), req.content)
        .await?;
    state.publish(UiEvent::MemoryChanged { agent_id: id });
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes one memory by key (URL-encoded).
#[utoipa::path(delete, path = "/v1/agents/{id}/memory/{key}", tag = "memory",
    params(("id" = String, Path, description = "Agent id (UUID)"),
        ("key" = String, Path, description = "Memory key")),
    responses((status = 204, description = "Deleted"),
        (status = 404, description = "Unknown agent or key", body = ErrorBody)))]
pub(crate) async fn delete_memory(
    State(state): State<AppState>,
    Path((id, key)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    if !state.store.forget(id, key.clone()).await? {
        return Err(ApiError::NotFound(format!("memory {key:?} not found")));
    }
    state.publish(UiEvent::MemoryChanged { agent_id: id });
    Ok(StatusCode::NO_CONTENT)
}
