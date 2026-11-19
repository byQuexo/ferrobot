//! Each agent's computer: state, sleep/wake, snapshots, exec.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use ferrobot_sandbox::{ExecOutput, ExecRequest, SandboxState, SleepLevel};
use ferrobot_store::SnapshotRecord;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::{ApiResult, parse_id};
use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::lifecycle::{self, handle_of};
use crate::views::{Presence, presence};

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct SandboxInfo {
    /// Backend kind, e.g. `docker`; null before provisioning.
    backend: Option<String>,
    /// Backend-specific id (container id, ...).
    sandbox_id: Option<String>,
    /// Live state reported by the backend; null if unknown or not provisioned.
    state: Option<SandboxState>,
    presence: Presence,
    connected: bool,
    /// Seconds since the last activity; pauses at `warm_after_s`, stops at
    /// `idle_timeout_s`.
    idle_s: u64,
    /// Warm sleep (pause) after this long idle; `null`: never.
    warm_after_s: Option<u64>,
    /// Cold sleep (stop) after this long idle; `null`: never.
    idle_timeout_s: Option<u64>,
    /// Whether the backend can snapshot and restore.
    snapshots_supported: bool,
}

/// Live sandbox status for the info panel.
#[utoipa::path(get, path = "/v1/agents/{id}/sandbox", tag = "sandbox",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = SandboxInfo),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn sandbox_info(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<SandboxInfo> {
    let agent = state.store.get_agent(parse_id(&id)?).await?;
    let live = match handle_of(&agent) {
        Some(h) => state.backend.state(&h).await.ok(),
        None => None,
    };
    let connected = state.hub.is_connected(agent.id);
    Ok(Json(SandboxInfo {
        presence: presence(&agent, connected, state.hub.is_working(agent.id)),
        connected,
        idle_s: state.hub.last_activity(agent.id).elapsed().as_secs(),
        warm_after_s: state.settings.warm_timeout.map(|d| d.as_secs()),
        idle_timeout_s: state.settings.idle_timeout.map(|d| d.as_secs()),
        snapshots_supported: state.backend.kind() == "docker",
        backend: agent.backend,
        sandbox_id: agent.sandbox_id,
        state: live,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct StopQuery {
    /// `cold` (default) stops the sandbox; `warm` pauses it, keeping memory,
    /// where the backend can (otherwise it sleeps cold).
    #[serde(default)]
    #[param(value_type = Option<SleepLevel>)]
    level: Option<SleepLevel>,
}

/// Puts the agent to sleep, keeping its disk. A new message or `wake`
/// starts (or resumes) it again.
#[utoipa::path(post, path = "/v1/agents/{id}/stop", tag = "sandbox",
    params(("id" = String, Path, description = "Agent id (UUID)"), StopQuery),
    responses((status = 204, description = "Sleeping"),
        (status = 404, description = "Unknown agent", body = ErrorBody),
        (status = 501, description = "The backend cannot pause (`level=warm`)", body = ErrorBody),
        (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn stop(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<StopQuery>,
) -> Result<StatusCode, ApiError> {
    let level = q.level.unwrap_or(SleepLevel::Cold);
    lifecycle::sleep(&state, parse_id(&id)?, "operator", level).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Starts (or re-creates) the agent's sandbox.
#[utoipa::path(post, path = "/v1/agents/{id}/wake", tag = "sandbox",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 204, description = "Running"),
        (status = 404, description = "Unknown agent", body = ErrorBody),
        (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn wake(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    lifecycle::wake(&state, parse_id(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Removes the sandbox but keeps the agent and its disk; `wake` recreates it.
#[utoipa::path(delete, path = "/v1/agents/{id}/sandbox", tag = "sandbox",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 204, description = "Sandbox removed"),
        (status = 404, description = "Unknown agent", body = ErrorBody),
        (status = 409, description = "No sandbox", body = ErrorBody),
        (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn destroy_sandbox(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    lifecycle::destroy_sandbox(&state, parse_id(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct ExecReq {
    /// argv; must not be empty.
    #[schema(example = json!(["sh", "-c", "ls"]))]
    cmd: Vec<String>,
    workdir: Option<String>,
}

/// Runs a command in the agent's computer (operator only, loopback API).
/// Wakes the computer if it sleeps.
#[utoipa::path(post, path = "/v1/agents/{id}/exec", tag = "sandbox",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    request_body = ExecReq,
    responses((status = 200, body = ExecOutput),
        (status = 400, body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody),
        (status = 409, description = "No sandbox", body = ErrorBody),
        (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn exec(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ExecReq>,
) -> ApiResult<ExecOutput> {
    if req.cmd.is_empty() {
        return Err(ApiError::BadRequest("cmd must not be empty".into()));
    }
    let id = parse_id(&id)?;
    // Operator commands count as activity and wake a sleeping computer.
    lifecycle::ensure_awake(&state, id).await?;
    state.hub.touch(id);
    let agent = state.store.get_agent(id).await?;
    let handle = lifecycle::sandbox_handle(&agent)?;
    let request = ExecRequest {
        cmd: req.cmd,
        workdir: req.workdir,
        ..ExecRequest::default()
    };
    Ok(Json(state.backend.exec(&handle, &request).await?))
}

/// Snapshots of the agent's sandbox, newest first.
#[utoipa::path(get, path = "/v1/agents/{id}/snapshots", tag = "sandbox",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = Vec<SnapshotRecord>),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn list_snapshots(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Vec<SnapshotRecord>> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    Ok(Json(state.store.list_snapshots(id).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct SnapshotReq {
    /// Free-form label (default `manual`, max 64 bytes).
    #[serde(default = "default_label")]
    label: String,
}

fn default_label() -> String {
    "manual".into()
}

/// Snapshots the sandbox (Docker: root filesystem + home directory).
#[utoipa::path(post, path = "/v1/agents/{id}/snapshots", tag = "sandbox",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    request_body = SnapshotReq,
    responses((status = 201, body = SnapshotRecord),
        (status = 400, body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody),
        (status = 409, description = "No sandbox", body = ErrorBody),
        (status = 501, description = "Backend cannot snapshot", body = ErrorBody),
        (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn create_snapshot(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SnapshotReq>,
) -> Result<(StatusCode, Json<SnapshotRecord>), ApiError> {
    let label = req.label.trim();
    if label.is_empty() || label.len() > 64 {
        return Err(ApiError::BadRequest("label must be 1-64 bytes".into()));
    }
    let id = parse_id(&id)?;
    state.hub.touch(id);
    let record = lifecycle::snapshot(&state, id, label).await?;
    Ok((StatusCode::CREATED, Json(record)))
}

fn parse_sid(raw: &str) -> Result<i64, ApiError> {
    raw.parse()
        .map_err(|_| ApiError::NotFound(format!("snapshot {raw} not found")))
}

/// Replaces the sandbox with one booted from the snapshot. The current
/// disk state is discarded; the agent's chat and memory are kept.
#[utoipa::path(post, path = "/v1/agents/{id}/snapshots/{sid}/restore", tag = "sandbox",
    params(("id" = String, Path, description = "Agent id (UUID)"),
        ("sid" = i64, Path, description = "Snapshot id")),
    responses((status = 204, description = "Restored and running"),
        (status = 404, description = "Unknown agent or snapshot", body = ErrorBody),
        (status = 409, description = "No sandbox or wrong backend", body = ErrorBody),
        (status = 501, description = "Backend cannot restore", body = ErrorBody),
        (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn restore_snapshot(
    State(state): State<AppState>,
    Path((id, sid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    state.hub.touch(id);
    lifecycle::restore(&state, id, parse_sid(&sid)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes a snapshot and its data.
#[utoipa::path(delete, path = "/v1/agents/{id}/snapshots/{sid}", tag = "sandbox",
    params(("id" = String, Path, description = "Agent id (UUID)"),
        ("sid" = i64, Path, description = "Snapshot id")),
    responses((status = 204, description = "Deleted"),
        (status = 404, description = "Unknown agent or snapshot", body = ErrorBody),
        (status = 501, description = "Backend cannot delete snapshots", body = ErrorBody),
        (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn delete_snapshot(
    State(state): State<AppState>,
    Path((id, sid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    lifecycle::delete_snapshot(&state, parse_id(&id)?, parse_sid(&sid)?).await?;
    Ok(StatusCode::NO_CONTENT)
}
