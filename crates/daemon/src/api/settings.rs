//! Daemon-wide operator settings (`/v1/settings`).

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ApiResult;
use crate::AppState;
use crate::error::ApiError;

const SETTING: &str = "general";

/// Daemon-wide settings. Omitted fields take their defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub(crate) struct GeneralSettings {
    /// Agents create agents without an approval card. The agent limit still applies.
    pub auto_mode: bool,
}

pub(crate) async fn load(state: &AppState) -> Result<GeneralSettings, ApiError> {
    match state.store.get_setting(SETTING).await? {
        Some(raw) => serde_json::from_str(&raw)
            .map_err(|e| ApiError::Internal(format!("general settings: {e}"))),
        None => Ok(GeneralSettings::default()),
    }
}

/// The daemon-wide settings.
#[utoipa::path(get, path = "/v1/settings", tag = "settings",
    responses((status = 200, body = GeneralSettings)))]
pub(crate) async fn get_settings(State(state): State<AppState>) -> ApiResult<GeneralSettings> {
    Ok(Json(load(&state).await?))
}

/// Replaces the daemon-wide settings.
#[utoipa::path(put, path = "/v1/settings", tag = "settings",
    request_body = GeneralSettings,
    responses((status = 200, body = GeneralSettings)))]
pub(crate) async fn put_settings(
    State(state): State<AppState>,
    Json(settings): Json<GeneralSettings>,
) -> ApiResult<GeneralSettings> {
    let raw = serde_json::to_string(&settings).map_err(|e| ApiError::Internal(e.to_string()))?;
    state.store.put_setting(SETTING, raw).await?;
    Ok(Json(settings))
}
