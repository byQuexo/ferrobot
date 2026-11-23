//! Per-agent and global long-term memory (the same store the MCP tools use).

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
    let key = check(&req)?;
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

fn check(req: &PutMemory) -> Result<&str, ApiError> {
    let key = req.key.trim();
    if key.is_empty() || key.len() > MAX_MEMORY_KEY_BYTES {
        return Err(ApiError::BadRequest(format!(
            "key must be 1-{MAX_MEMORY_KEY_BYTES} bytes"
        )));
    }
    check_len("content", &req.content)?;
    Ok(key)
}

/// Lists or searches the global memory all agents share.
#[utoipa::path(get, path = "/v1/memory", tag = "memory",
    params(MemoryQuery),
    responses((status = 200, body = Vec<MemoryEntry>)))]
pub(crate) async fn list_global_memory(
    State(state): State<AppState>,
    Query(q): Query<MemoryQuery>,
) -> ApiResult<Vec<MemoryEntry>> {
    let limit = q.limit.unwrap_or(100).clamp(1, 200);
    Ok(Json(state.store.recall_global(q.q, limit).await?))
}

/// Creates or replaces a global memory.
#[utoipa::path(post, path = "/v1/memory", tag = "memory",
    request_body = PutMemory,
    responses((status = 204, description = "Stored"),
        (status = 400, body = ErrorBody)))]
pub(crate) async fn put_global_memory(
    State(state): State<AppState>,
    Json(req): Json<PutMemory>,
) -> Result<StatusCode, ApiError> {
    let key = check(&req)?.to_owned();
    state.store.remember_global(key, req.content, None).await?;
    state.publish(UiEvent::GlobalMemoryChanged);
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes one global memory by key (URL-encoded).
#[utoipa::path(delete, path = "/v1/memory/{key}", tag = "memory",
    params(("key" = String, Path, description = "Memory key")),
    responses((status = 204, description = "Deleted"),
        (status = 404, description = "Unknown key", body = ErrorBody)))]
pub(crate) async fn delete_global_memory(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !state.store.forget_global(key.clone(), None).await? {
        return Err(ApiError::NotFound(format!("memory {key:?} not found")));
    }
    state.publish(UiEvent::GlobalMemoryChanged);
    Ok(StatusCode::NO_CONTENT)
}

/// Where memories live: an Obsidian vault folder, or the built-in memory.
#[derive(Debug, serde::Serialize, ToSchema)]
pub(crate) struct MemoryVaultView {
    /// The vault folder; memories are Markdown notes under its `ferrobot/` folder. `null`: the
    /// built-in memory.
    path: Option<String>,
    /// The folder is an Obsidian vault (it has `.obsidian`).
    obsidian: bool,
    /// After a change: how many stored memories were copied into the vault.
    copied: Option<usize>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct SetMemoryVault {
    /// An existing folder (absolute, or starting with `~/`); `null` goes back to the built-in
    /// memory, which still holds what it had before.
    path: Option<String>,
}

fn vault_view(path: Option<std::path::PathBuf>, copied: Option<usize>) -> MemoryVaultView {
    MemoryVaultView {
        obsidian: path.as_ref().is_some_and(|p| p.join(".obsidian").is_dir()),
        path: path.map(|p| p.display().to_string()),
        copied,
    }
}

/// Where memories live.
#[utoipa::path(get, path = "/v1/memory-vault", tag = "memory",
    responses((status = 200, body = MemoryVaultView)))]
pub(crate) async fn get_memory_vault(State(state): State<AppState>) -> ApiResult<MemoryVaultView> {
    Ok(Json(vault_view(state.store.memory_vault().await?, None)))
}

/// Keeps memories in an Obsidian vault folder, or back in the built-in memory. Memories stored
/// so far are copied into the vault (notes with the same key are kept).
#[utoipa::path(put, path = "/v1/memory-vault", tag = "memory",
    request_body = SetMemoryVault,
    responses((status = 200, body = MemoryVaultView),
        (status = 400, description = "Not an existing folder", body = ErrorBody)))]
pub(crate) async fn put_memory_vault(
    State(state): State<AppState>,
    Json(req): Json<SetMemoryVault>,
) -> ApiResult<MemoryVaultView> {
    let path = match req.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        None => None,
        Some(raw) => {
            let path = match raw.strip_prefix("~/") {
                Some(rest) => std::env::var_os("HOME")
                    .map(|home| std::path::PathBuf::from(home).join(rest))
                    .ok_or_else(|| ApiError::BadRequest("HOME is not set".into()))?,
                None => std::path::PathBuf::from(raw),
            };
            if raw.len() > 1024 || !path.is_absolute() || !path.is_dir() {
                return Err(ApiError::BadRequest(
                    "the vault must be an existing folder, e.g. ~/Obsidian/Notes".into(),
                ));
            }
            Some(path)
        }
    };
    let copied = state.store.set_memory_vault(path.clone()).await?;
    state.publish(UiEvent::GlobalMemoryChanged);
    Ok(Json(vault_view(path, Some(copied))))
}
