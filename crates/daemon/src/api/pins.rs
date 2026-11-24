//! Replies and shared files the operator pinned, listed behind the sidebar's pin button
//! (`/v1/pins`). Agents are pinned on the agent itself (`pinned`).

use std::collections::HashSet;

use axum::Json;
use axum::extract::State;
use ferrobot_core::AgentId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ApiResult;
use crate::AppState;
use crate::error::{ApiError, ErrorBody};

const SETTING: &str = "pins";
const MAX_PINS: usize = 100;
const MAX_LABEL: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PinKind {
    Message,
    File,
}

/// A pinned reply or shared file, in the pin list's order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub(crate) struct Pin {
    pub kind: PinKind,
    /// The agent whose chat holds it.
    #[schema(value_type = String)]
    pub agent_id: AgentId,
    /// The reply's `assistant_text` event, or the file's `file` event.
    pub event: i64,
    /// Shown in the pin list; 1-120 characters.
    pub label: String,
}

async fn load(state: &AppState) -> Result<Vec<Pin>, ApiError> {
    let Some(raw) = state.store.get_setting(SETTING).await? else {
        return Ok(Vec::new());
    };
    let pins: Vec<Pin> =
        serde_json::from_str(&raw).map_err(|e| ApiError::Internal(format!("pins: {e}")))?;
    let agents: HashSet<AgentId> = state
        .store
        .list_agents()
        .await?
        .into_iter()
        .map(|a| a.id)
        .collect();
    // A deleted agent takes its pins along.
    Ok(pins
        .into_iter()
        .filter(|p| agents.contains(&p.agent_id))
        .collect())
}

/// The pinned replies and files, in order.
#[utoipa::path(get, path = "/v1/pins", tag = "chat",
    responses((status = 200, body = [Pin])))]
pub(crate) async fn get_pins(State(state): State<AppState>) -> ApiResult<Vec<Pin>> {
    Ok(Json(load(&state).await?))
}

/// Replaces the pin list: pinning, renaming, reordering and unpinning all send the whole list.
#[utoipa::path(put, path = "/v1/pins", tag = "chat",
    request_body = [Pin],
    responses((status = 200, body = [Pin]),
        (status = 400, description = "Too many pins, a bad label or an unknown agent", body = ErrorBody)))]
pub(crate) async fn put_pins(
    State(state): State<AppState>,
    Json(pins): Json<Vec<Pin>>,
) -> ApiResult<Vec<Pin>> {
    if pins.len() > MAX_PINS {
        return Err(ApiError::BadRequest(format!("at most {MAX_PINS} pins")));
    }
    let agents: HashSet<AgentId> = state
        .store
        .list_agents()
        .await?
        .into_iter()
        .map(|a| a.id)
        .collect();
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(pins.len());
    for mut pin in pins {
        let label = pin.label.trim().to_owned();
        pin.label = label;
        let label_ok = (1..=MAX_LABEL).contains(&pin.label.chars().count())
            && !pin.label.chars().any(char::is_control);
        if !label_ok {
            return Err(ApiError::BadRequest(format!(
                "a pin's label has 1-{MAX_LABEL} printable characters"
            )));
        }
        if pin.event < 1 || !agents.contains(&pin.agent_id) {
            return Err(ApiError::BadRequest(
                "a pin names an unknown agent or event".into(),
            ));
        }
        if seen.insert((pin.kind, pin.agent_id, pin.event)) {
            out.push(pin);
        }
    }
    let raw = serde_json::to_string(&out).map_err(|e| ApiError::Internal(e.to_string()))?;
    state.store.put_setting(SETTING, raw).await?;
    Ok(Json(out))
}
