//! HTML files the operator keeps as dashboards (`/v1/dashboards`). A dashboard follows a path
//! in an agent's computer, so it always shows the newest version the agent shared.

use std::collections::HashSet;

use axum::Json;
use axum::extract::State;
use ferrobot_core::AgentId;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use super::settings::{load_json, save_json};
use super::{ApiResult, agent_ids, is_printable};
use crate::AppState;
use crate::error::{ApiError, ErrorBody};

const SETTING: &str = "dashboards";
const MAX_DASHBOARDS: usize = 20;
const MAX_LABEL: usize = 60;

/// A dashboard: a shared HTML file, by its agent and path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub(crate) struct Dashboard {
    #[schema(value_type = String)]
    pub agent_id: AgentId,
    /// The file's path in the agent's computer, e.g. `/home/agent/finance.html`.
    pub path: String,
    /// Shown on its tab; 1-60 characters.
    pub label: String,
    /// Read only (ignored when saving): the newest shared version's `file` event; `null`
    /// until one is shared.
    #[serde(default)]
    pub file: Option<i64>,
    /// Read only (ignored when saving): when that version was shared.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub updated_at: Option<OffsetDateTime>,
}

async fn resolve(state: &AppState, list: Vec<Dashboard>) -> Result<Vec<Dashboard>, ApiError> {
    let agents = agent_ids(state).await?;
    let mut out = Vec::with_capacity(list.len());
    // A deleted agent takes its dashboards along.
    for mut d in list.into_iter().filter(|d| agents.contains(&d.agent_id)) {
        let newest = state.store.latest_file(d.agent_id, d.path.clone()).await?;
        (d.file, d.updated_at) = newest.map_or((None, None), |(id, at)| (Some(id), Some(at)));
        out.push(d);
    }
    Ok(out)
}

/// The dashboards, in order, each with its newest shared version.
#[utoipa::path(get, path = "/v1/dashboards", tag = "chat",
    responses((status = 200, body = [Dashboard])))]
pub(crate) async fn get_dashboards(State(state): State<AppState>) -> ApiResult<Vec<Dashboard>> {
    let list = load_json(&state, SETTING, "dashboards").await?;
    Ok(Json(resolve(&state, list).await?))
}

/// Replaces the dashboards: adding, renaming, reordering and removing all send the whole list.
#[utoipa::path(put, path = "/v1/dashboards", tag = "chat",
    request_body = [Dashboard],
    responses((status = 200, body = [Dashboard]),
        (status = 400, description = "Too many, a bad label or path, or an unknown agent", body = ErrorBody)))]
pub(crate) async fn put_dashboards(
    State(state): State<AppState>,
    Json(list): Json<Vec<Dashboard>>,
) -> ApiResult<Vec<Dashboard>> {
    if list.len() > MAX_DASHBOARDS {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_DASHBOARDS} dashboards"
        )));
    }
    let agents = agent_ids(&state).await?;
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(list.len());
    for mut d in list {
        let label = d.label.trim().to_owned();
        d.label = label;
        if !is_printable(&d.label, MAX_LABEL) {
            return Err(ApiError::BadRequest(format!(
                "a dashboard's name has 1-{MAX_LABEL} printable characters"
            )));
        }
        let html = std::path::Path::new(&d.path)
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("html") || x.eq_ignore_ascii_case("htm"));
        if d.path.len() > 1024 || d.path.contains('\0') || !html {
            return Err(ApiError::BadRequest("a dashboard is an .html file".into()));
        }
        if !agents.contains(&d.agent_id) {
            return Err(ApiError::BadRequest(
                "a dashboard names an unknown agent".into(),
            ));
        }
        if seen.insert((d.agent_id, d.path.clone())) {
            out.push(Dashboard {
                file: None,
                updated_at: None,
                ..d
            });
        }
    }
    save_json(&state, SETTING, &out).await?;
    Ok(Json(resolve(&state, out).await?))
}
