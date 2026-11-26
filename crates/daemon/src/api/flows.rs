//! `/v1/flows`: flows (proof of concept), kept in the `flows` setting, and their runs.

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use ferrobot_core::AgentId;

use super::ApiResult;
use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::flows::{self, Flow, FlowRun, Step};

const SETTING: &str = "flows";
const MAX_FLOWS: usize = 20;
const MAX_STEPS: usize = 10;
const MAX_NAME: usize = 60;
const MAX_PROMPT_BYTES: usize = 4000;
const MAX_CONTAINS: usize = 200;

async fn load(state: &AppState) -> Result<Vec<Flow>, ApiError> {
    match state.store.get_setting(SETTING).await? {
        Some(raw) => {
            serde_json::from_str(&raw).map_err(|e| ApiError::Internal(format!("flows: {e}")))
        }
        None => Ok(Vec::new()),
    }
}

/// The flows, in order.
#[utoipa::path(get, path = "/v1/flows", tag = "flows",
    responses((status = 200, body = [Flow])))]
pub(crate) async fn get_flows(State(state): State<AppState>) -> ApiResult<Vec<Flow>> {
    Ok(Json(load(&state).await?))
}

fn check(flow: &mut Flow, agents: &HashSet<AgentId>) -> Result<(), String> {
    let id_ok = (1..=64).contains(&flow.id.len())
        && flow
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !id_ok {
        return Err("a flow id is 1-64 letters, digits or '-'".into());
    }
    flow.name = flow.name.trim().to_owned();
    if !(1..=MAX_NAME).contains(&flow.name.chars().count())
        || flow.name.chars().any(char::is_control)
    {
        return Err(format!(
            "a flow's name has 1-{MAX_NAME} printable characters"
        ));
    }
    if !(1..=MAX_STEPS).contains(&flow.steps.len()) {
        return Err(format!("a flow has 1-{MAX_STEPS} steps"));
    }
    for step in &mut flow.steps {
        let agent = match step {
            Step::Agent { agent, prompt } => {
                *prompt = prompt.trim().to_owned();
                if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
                    return Err(format!("a step's prompt has 1-{MAX_PROMPT_BYTES} bytes"));
                }
                Some(*agent)
            }
            Step::Condition { contains } => {
                *contains = contains.trim().to_owned();
                if !(1..=MAX_CONTAINS).contains(&contains.chars().count()) {
                    return Err(format!("a condition has 1-{MAX_CONTAINS} characters"));
                }
                None
            }
            Step::Message { agent } => Some(*agent),
        };
        if agent.is_some_and(|a| !agents.contains(&a)) {
            return Err("a step names an unknown agent".into());
        }
    }
    Ok(())
}

/// Replaces the flows: adding, editing, reordering and removing all send the whole list.
#[utoipa::path(put, path = "/v1/flows", tag = "flows",
    request_body = [Flow],
    responses((status = 200, body = [Flow]),
        (status = 400, description = "Too many, a bad id, name, step or an unknown agent", body = ErrorBody)))]
pub(crate) async fn put_flows(
    State(state): State<AppState>,
    Json(mut list): Json<Vec<Flow>>,
) -> ApiResult<Vec<Flow>> {
    if list.len() > MAX_FLOWS {
        return Err(ApiError::BadRequest(format!("at most {MAX_FLOWS} flows")));
    }
    let agents: HashSet<AgentId> = state
        .store
        .list_agents()
        .await?
        .into_iter()
        .map(|a| a.id)
        .collect();
    let mut ids = HashSet::new();
    for flow in &mut list {
        check(flow, &agents).map_err(ApiError::BadRequest)?;
        if !ids.insert(flow.id.clone()) {
            return Err(ApiError::BadRequest("flow ids must be unique".into()));
        }
    }
    let raw = serde_json::to_string(&list).map_err(|e| ApiError::Internal(e.to_string()))?;
    state.store.put_setting(SETTING, raw).await?;
    Ok(Json(list))
}

/// Starts a run of the flow; `flow_run` events follow it.
#[utoipa::path(post, path = "/v1/flows/{id}/run", tag = "flows",
    params(("id" = String, Path, description = "Flow id")),
    responses((status = 202, body = FlowRun),
        (status = 404, description = "Unknown flow", body = ErrorBody)))]
pub(crate) async fn run_flow(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<FlowRun>), ApiError> {
    let flow = load(&state)
        .await?
        .into_iter()
        .find(|f| f.id == id)
        .ok_or_else(|| ApiError::NotFound("no such flow".into()))?;
    Ok((StatusCode::ACCEPTED, Json(flows::start(&state, flow))))
}

/// Runs since the daemon started, newest first (at most 50).
#[utoipa::path(get, path = "/v1/flows/runs", tag = "flows",
    responses((status = 200, body = [FlowRun])))]
pub(crate) async fn list_flow_runs(State(state): State<AppState>) -> ApiResult<Vec<FlowRun>> {
    Ok(Json(flows::runs(&state)))
}
