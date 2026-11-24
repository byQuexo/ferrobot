//! Trace export to an OTLP/HTTP endpoint (`/v1/otel`).

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ApiResult;
use super::providers::{check_key, clean};
use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::otel::{self, ExportStatus, OtelSettings};

/// The export settings, whether the header's value is stored, and how the last export went.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct OtelView {
    #[serde(flatten)]
    settings: OtelSettings,
    /// The auth header's value is in the secret store; it is never returned.
    has_secret: bool,
    last_export: Option<ExportStatus>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct OtelSecret {
    /// The auth header's value, e.g. `Bearer …` or `Basic …`; `null` removes it.
    value: Option<String>,
}

async fn view(state: &AppState, settings: OtelSettings) -> ApiResult<OtelView> {
    Ok(Json(OtelView {
        settings,
        has_secret: state.settings.secrets.get(otel::SECRET).await?.is_some(),
        last_export: otel::last_export(),
    }))
}

/// Where finished turns' traces go.
#[utoipa::path(get, path = "/v1/otel", tag = "settings",
    responses((status = 200, body = OtelView)))]
pub(crate) async fn get_otel(State(state): State<AppState>) -> ApiResult<OtelView> {
    let settings = otel::load(&state).await?;
    view(&state, settings).await
}

/// Replaces the export settings. Each turn's trace then goes to the endpoint when the turn ends.
#[utoipa::path(put, path = "/v1/otel", tag = "settings",
    request_body = OtelSettings,
    responses((status = 200, body = OtelView),
        (status = 400, description = "Invalid endpoint or header name", body = ErrorBody)))]
pub(crate) async fn put_otel(
    State(state): State<AppState>,
    Json(mut settings): Json<OtelSettings>,
) -> ApiResult<OtelView> {
    otel::validate(&mut settings).map_err(ApiError::BadRequest)?;
    otel::save(&state, &settings).await?;
    view(&state, settings).await
}

/// Stores or removes the auth header's value, kept in the daemon's secret store.
#[utoipa::path(put, path = "/v1/otel/secret", tag = "settings",
    request_body = OtelSecret,
    responses((status = 200, body = OtelView), (status = 400, body = ErrorBody)))]
pub(crate) async fn put_otel_secret(
    State(state): State<AppState>,
    Json(req): Json<OtelSecret>,
) -> ApiResult<OtelView> {
    let secrets = &state.settings.secrets;
    match clean(req.value) {
        Some(value) => {
            check_key(&value)?;
            secrets.set(otel::SECRET, &value).await?;
        }
        None => secrets.delete(otel::SECRET).await?,
    }
    let settings = otel::load(&state).await?;
    view(&state, settings).await
}
