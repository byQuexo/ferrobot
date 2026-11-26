//! Daemon-wide operator settings (`/v1/settings`).

use axum::Json;
use axum::extract::State;
use serde::de::DeserializeOwned;
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
    /// How agents name the agents they create, in the operator's words (e.g. "Greek gods");
    /// `null`: short role names. 1-300 characters.
    pub agent_names: Option<String>,
}

/// Longest naming convention.
const MAX_AGENT_NAMES: usize = 300;

/// The JSON setting `key`, its default when unset; `what` names it in errors.
pub(crate) async fn load_json<T: DeserializeOwned + Default>(
    state: &AppState,
    key: &str,
    what: &str,
) -> Result<T, ApiError> {
    match state.store.get_setting(key).await? {
        Some(raw) => {
            serde_json::from_str(&raw).map_err(|e| ApiError::Internal(format!("{what}: {e}")))
        }
        None => Ok(T::default()),
    }
}

pub(crate) async fn save_json(
    state: &AppState,
    key: &str,
    value: &impl Serialize,
) -> Result<(), ApiError> {
    let raw = serde_json::to_string(value).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(state.store.put_setting(key, raw).await?)
}

pub(crate) async fn load(state: &AppState) -> Result<GeneralSettings, ApiError> {
    load_json(state, SETTING, "general settings").await
}

/// Unreadable settings fail closed: the operator gets the card.
pub(crate) async fn auto_mode(state: &AppState) -> bool {
    load(state).await.is_ok_and(|s| s.auto_mode)
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
    responses((status = 200, body = GeneralSettings),
        (status = 400, description = "Naming convention too long", body = crate::error::ErrorBody)))]
pub(crate) async fn put_settings(
    State(state): State<AppState>,
    Json(mut settings): Json<GeneralSettings>,
) -> ApiResult<GeneralSettings> {
    settings.agent_names = settings
        .agent_names
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty());
    if settings
        .agent_names
        .as_deref()
        .is_some_and(|n| !super::is_printable(n, MAX_AGENT_NAMES))
    {
        return Err(ApiError::BadRequest(format!(
            "agent_names must be 1-{MAX_AGENT_NAMES} printable characters"
        )));
    }
    let renamed = load(&state).await?.agent_names != settings.agent_names;
    save_json(&state, SETTING, &settings).await?;
    if renamed {
        crate::agents::reconfigure_creators(&state).await;
    }
    Ok(Json(settings))
}
