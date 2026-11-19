//! Operator-facing HTTP + WebSocket API (`/v1`), used by the desktop UI and
//! any other client. Documented in `protocol/openapi.json`.

pub(crate) mod agents;
pub(crate) mod chat;
pub(crate) mod memory;
pub(crate) mod routines;
pub(crate) mod sandbox;
pub(crate) mod ws;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use ferrobot_core::AgentId;
use serde::Serialize;
use utoipa::ToSchema;

use crate::AppState;
use crate::error::ApiError;

pub(crate) type ApiResult<T> = Result<Json<T>, ApiError>;

/// Upper bound for operator-provided texts (bytes).
pub(crate) const MAX_TEXT_BYTES: usize = 32 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/ws", get(ws::ui_ws))
        .route(
            "/v1/agents",
            get(agents::list_agents).post(agents::create_agent),
        )
        .route(
            "/v1/agents/{id}",
            get(agents::get_agent)
                .patch(agents::update_agent)
                .delete(agents::delete_agent),
        )
        .route(
            "/v1/agents/{id}/avatar",
            get(agents::get_avatar)
                .put(agents::put_avatar)
                .delete(agents::delete_avatar),
        )
        .route(
            "/v1/agents/{id}/messages",
            get(chat::list_messages).post(chat::post_message),
        )
        .route("/v1/agents/{id}/events", get(chat::list_events))
        .route("/v1/agents/{id}/read", post(chat::mark_read))
        .route("/v1/agents/{id}/interrupt", post(chat::interrupt))
        .route(
            "/v1/agents/{id}/memory",
            get(memory::list_memory).post(memory::put_memory),
        )
        .route(
            "/v1/agents/{id}/memory/{key}",
            axum::routing::delete(memory::delete_memory),
        )
        .route(
            "/v1/agents/{id}/routines",
            get(routines::list_routines).post(routines::create_routine),
        )
        .route(
            "/v1/routines/{rid}",
            axum::routing::patch(routines::update_routine).delete(routines::delete_routine),
        )
        .route("/v1/routines/{rid}/run", post(routines::run_routine))
        .route("/v1/routines/{rid}/runs", get(routines::list_runs))
        .route(
            "/v1/agents/{id}/sandbox",
            get(sandbox::sandbox_info).delete(sandbox::destroy_sandbox),
        )
        .route("/v1/agents/{id}/stop", post(sandbox::stop))
        .route("/v1/agents/{id}/wake", post(sandbox::wake))
        .route("/v1/agents/{id}/exec", post(sandbox::exec))
        .route(
            "/v1/agents/{id}/snapshots",
            get(sandbox::list_snapshots).post(sandbox::create_snapshot),
        )
        .route(
            "/v1/agents/{id}/snapshots/{sid}",
            axum::routing::delete(sandbox::delete_snapshot),
        )
        .route(
            "/v1/agents/{id}/snapshots/{sid}/restore",
            post(sandbox::restore_snapshot),
        )
}

pub(crate) fn parse_id(raw: &str) -> Result<AgentId, ApiError> {
    raw.parse()
        .map_err(|_| ApiError::NotFound(format!("agent {raw} not found")))
}

/// Rejects texts over [`MAX_TEXT_BYTES`].
pub(crate) fn check_len(field: &str, value: &str) -> Result<(), ApiError> {
    if value.len() > MAX_TEXT_BYTES {
        return Err(ApiError::BadRequest(format!(
            "{field} exceeds {MAX_TEXT_BYTES} bytes"
        )));
    }
    Ok(())
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Health {
    name: &'static str,
    version: &'static str,
    backend: &'static str,
    driver: String,
    /// Whether real model calls are possible (driver `claude` with a key).
    model_proxy: bool,
    /// Seconds of inactivity before a sandbox sleeps; null when disabled.
    idle_timeout_s: Option<u64>,
    /// Default time zone for new routines.
    timezone: String,
}

/// Daemon identity and configuration summary.
#[utoipa::path(get, path = "/v1/health", tag = "system",
    responses((status = 200, body = Health)))]
pub(crate) async fn health(State(state): State<AppState>) -> Json<Health> {
    let s = &state.settings;
    Json(Health {
        name: ferrobot_core::PROJECT_NAME,
        version: env!("CARGO_PKG_VERSION"),
        backend: state.backend.kind(),
        driver: s.driver.clone(),
        model_proxy: s.anthropic_api_key.is_some(),
        idle_timeout_s: s.idle_timeout.map(|d| d.as_secs()),
        timezone: s.default_timezone.clone(),
    })
}
