//! Versions, setup and the latest log lines for bug reports (`/v1/diagnostics`).

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::{ApiResult, BACKEND_TIMEOUT, sandbox_error};
use crate::AppState;
use crate::logs::MAX_LINES;

/// What a bug report needs about this daemon. Never holds a key or token.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Diagnostics {
    version: &'static str,
    /// `debug` or `release`.
    build: &'static str,
    /// Operating system, e.g. `macos`, `linux`, `windows`.
    os: &'static str,
    /// CPU architecture, e.g. `aarch64`, `x86_64`.
    arch: &'static str,
    backend: &'static str,
    driver: String,
    /// Directory with the database and the API token file.
    data_dir: String,
    /// Where snapshot archives go: `local`, or the scheme of `--storage-url` (`s3`, ...).
    storage: String,
    /// Whether the sandbox backend can start agents now (Docker: the engine answers).
    sandbox_ready: bool,
    /// Why the backend is not ready; null when ready.
    sandbox_error: Option<String>,
    /// Image sandboxes start from by default; null: the backend has no images.
    runner_image: Option<String>,
    /// That image's id; null when the engine does not have it or did not answer.
    runner_image_id: Option<String>,
    /// Whether the daemon has its own `ANTHROPIC_API_KEY` for agents without a provider.
    anthropic_key: bool,
    agents: usize,
    providers: usize,
    connectors: usize,
    /// Seconds since the daemon started.
    uptime_s: u64,
}

/// Versions, setup and counts for a bug report.
#[utoipa::path(get, path = "/v1/diagnostics", tag = "system",
    responses((status = 200, body = Diagnostics)))]
pub(crate) async fn get_diagnostics(State(state): State<AppState>) -> ApiResult<Diagnostics> {
    let s = &state.settings;
    let image = tokio::time::timeout(BACKEND_TIMEOUT, state.backend.default_image());
    let (sandbox_error, image) = tokio::join!(sandbox_error(&state), image);
    let (runner_image, runner_image_id) = image.ok().flatten().unzip();
    Ok(Json(Diagnostics {
        version: env!("CARGO_PKG_VERSION"),
        build: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        backend: state.backend.kind(),
        driver: s.driver.clone(),
        data_dir: s.data_dir.display().to_string(),
        storage: s.storage_kind.clone(),
        sandbox_ready: sandbox_error.is_none(),
        sandbox_error,
        runner_image,
        runner_image_id: runner_image_id.flatten(),
        anthropic_key: s.anthropic_api_key.is_some(),
        agents: state.store.list_agents().await?.len(),
        providers: state.store.list_providers().await?.len(),
        connectors: state.store.list_connectors().await?.len(),
        uptime_s: state.started.elapsed().as_secs(),
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct LogQuery {
    /// How many of the latest lines (default 200, at most 2000).
    #[serde(default = "default_lines")]
    lines: usize,
}

fn default_lines() -> usize {
    200
}

/// The latest daemon log lines.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct DiagnosticsLog {
    /// Oldest first, without colour codes; what looks like a credential reads `[REDACTED]`.
    lines: Vec<String>,
}

/// The latest daemon log lines, kept in memory since the daemon started.
#[utoipa::path(get, path = "/v1/diagnostics/log", tag = "system", params(LogQuery),
    responses((status = 200, body = DiagnosticsLog)))]
pub(crate) async fn get_log(
    State(state): State<AppState>,
    Query(q): Query<LogQuery>,
) -> Json<DiagnosticsLog> {
    Json(DiagnosticsLog {
        lines: state.settings.log.tail(q.lines.min(MAX_LINES)),
    })
}
