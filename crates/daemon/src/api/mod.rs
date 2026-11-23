//! The operator API (`/v1`, HTTP and WebSocket), documented in `protocol/openapi.json`.

pub(crate) mod agents;
pub(crate) mod chat;
pub(crate) mod clis;
pub(crate) mod connectors;
pub(crate) mod diagnostics;
pub(crate) mod files;
pub(crate) mod groups;
pub(crate) mod memory;
pub(crate) mod providers;
pub(crate) mod routines;
pub(crate) mod sandbox;
pub(crate) mod settings;
pub(crate) mod update;
pub(crate) mod usage;
pub(crate) mod voice;
pub(crate) mod ws;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::{get, post, put};
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
        .merge(integrations())
        .route("/v1/health", get(health))
        .merge(files::router())
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
        .route("/v1/conversations", get(chat::list_conversations))
        .route("/v1/conversations/{a}/{b}", get(chat::conversation_thread))
        .route("/v1/agents/{id}/interrupt", post(chat::interrupt))
        .route("/v1/agents/{id}/retry", post(chat::retry))
        .route("/v1/agents/{id}/new-session", post(chat::new_session))
        .route("/v1/agents/{id}/context", get(usage::get_context))
        .route(
            "/v1/agents/{id}/questions/{qid}/answer",
            post(chat::answer_question),
        )
        .route(
            "/v1/agents/{id}/memory",
            get(memory::list_memory).post(memory::put_memory),
        )
        .route(
            "/v1/agents/{id}/memory/{key}",
            axum::routing::delete(memory::delete_memory),
        )
        .route(
            "/v1/memory",
            get(memory::list_global_memory).post(memory::put_global_memory),
        )
        .route(
            "/v1/memory/{key}",
            axum::routing::delete(memory::delete_global_memory),
        )
        .route(
            "/v1/agents/{id}/routines",
            get(routines::list_routines).post(routines::create_routine),
        )
        .route(
            "/v1/routines/{rid}",
            axum::routing::patch(routines::update_routine).delete(routines::delete_routine),
        )
        .route(
            "/v1/settings",
            get(settings::get_settings).put(settings::put_settings),
        )
        .route(
            "/v1/update",
            get(update::get_update).post(update::start_update),
        )
        .route("/v1/diagnostics", get(diagnostics::get_diagnostics))
        .route("/v1/diagnostics/log", get(diagnostics::get_log))
        .route("/v1/usage", get(usage::get_usage))
        .route("/v1/limits", get(usage::get_limits))
        .route(
            "/v1/usage/prices",
            get(usage::get_prices).put(usage::put_prices),
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

/// Model providers, connectors, CLIs and voice.
fn integrations() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/providers",
            get(providers::list_providers).post(providers::create_provider),
        )
        .route(
            "/v1/providers/{id}",
            axum::routing::patch(providers::update_provider).delete(providers::delete_provider),
        )
        .route("/v1/providers/{id}/models", get(providers::provider_models))
        .route("/v1/claude-code", get(providers::claude_code_status))
        .route(
            "/v1/provider-kinds/{kind}/models",
            get(providers::kind_models),
        )
        .route(
            "/v1/providers/{id}/chatgpt-login",
            get(providers::chatgpt_login_status).post(providers::start_chatgpt_login),
        )
        .route(
            "/v1/connectors",
            get(connectors::list_connectors).post(connectors::create_connector),
        )
        .route(
            "/v1/connectors/{id}",
            axum::routing::patch(connectors::update_connector).delete(connectors::delete_connector),
        )
        .route(
            "/v1/agents/{id}/connectors",
            get(connectors::agent_connectors).put(connectors::set_agent_connectors),
        )
        .route(
            "/v1/connectors/{id}/oauth",
            post(connectors::start_oauth).delete(connectors::sign_out),
        )
        .route(
            "/v1/connectors/{id}/check",
            post(connectors::check_connector),
        )
        .route("/v1/oauth/callback", get(connectors::oauth_callback))
        .route("/v1/clis", get(clis::get_clis).put(clis::put_clis))
        .route("/v1/voice", get(voice::get_voice).put(voice::put_voice))
        .route(
            "/v1/voice/speech",
            post(voice::speech).get(voice::speech_link),
        )
        .route("/v1/voice/catalog", get(voice::catalog))
        .route("/v1/voice/keys/{provider}", put(voice::put_voice_key))
        .route("/v1/voice/live", post(voice::live))
        .route("/v1/call", get(voice::get_call).put(voice::put_call))
        .route("/v1/call/messages", post(voice::post_call_message))
        .route("/v1/rooms", get(voice::list_rooms))
        .route("/v1/rooms/{id}", get(voice::get_room))
        .route(
            "/v1/groups",
            get(groups::list_groups).post(groups::create_group),
        )
        .route(
            "/v1/groups/{id}",
            axum::routing::patch(groups::update_group).delete(groups::delete_group),
        )
        .route("/v1/groups/{id}/messages", post(groups::post_group_message))
        .route("/v1/groups/{id}/call", post(groups::call_group))
        .route(
            "/v1/voice/transcription",
            post(voice::transcription).layer(axum::extract::DefaultBodyLimit::max(
                crate::voice::MAX_AUDIO_BYTES,
            )),
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

/// How long a request waits for the sandbox backend, so a hanging engine cannot stall it.
pub(crate) const BACKEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Why the sandbox backend cannot start agents now; `None` when it can.
pub(crate) async fn sandbox_error(state: &AppState) -> Option<String> {
    match tokio::time::timeout(BACKEND_TIMEOUT, state.backend.ready()).await {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(_) => Some("the sandbox backend did not answer within 2 s".to_owned()),
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Health {
    name: &'static str,
    version: &'static str,
    backend: &'static str,
    driver: String,
    /// Whether agents make real model calls through the proxy (any driver
    /// but `mock`); each still needs a provider or `ANTHROPIC_API_KEY`.
    model_proxy: bool,
    /// Seconds idle before a sandbox is stopped (cold sleep); null: never.
    idle_timeout_s: Option<u64>,
    /// Default time zone for new routines.
    timezone: String,
    /// Whether the sandbox backend can start agents now (Docker: the engine answers).
    sandbox_ready: bool,
    /// Why the backend is not ready, for the operator; null when ready.
    sandbox_error: Option<String>,
    /// Whether the daemon has its own `ANTHROPIC_API_KEY` for agents without a provider.
    anthropic_key: bool,
}

/// Daemon identity and configuration summary.
#[utoipa::path(get, path = "/v1/health", tag = "system", security(()),
    responses((status = 200, body = Health)))]
pub(crate) async fn health(State(state): State<AppState>) -> Json<Health> {
    let s = &state.settings;
    let sandbox_error = sandbox_error(&state).await;
    Json(Health {
        name: ferrobot_core::PROJECT_NAME,
        version: env!("CARGO_PKG_VERSION"),
        backend: state.backend.kind(),
        driver: s.driver.clone(),
        model_proxy: s.driver != "mock",
        idle_timeout_s: s.idle_timeout.map(|d| d.as_secs()),
        timezone: s.default_timezone.clone(),
        sandbox_ready: sandbox_error.is_none(),
        sandbox_error,
        anthropic_key: s.anthropic_api_key.is_some(),
    })
}
