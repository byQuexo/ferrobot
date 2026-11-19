//! Routines: scheduled prompts per agent.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use ferrobot_core::MailMessage;
use ferrobot_store::{NewRoutine, Routine, RoutineRun, RunTrigger};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiResult, check_len, parse_id};
use crate::error::{ApiError, ErrorBody};
use crate::scheduler::{self, Schedule};
use crate::{AppState, UiEvent};

/// A routine plus a readable form of its schedule.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct RoutineView {
    #[serde(flatten)]
    routine: Routine,
    /// English description of the schedule, e.g. "At 09:00 on Monday through Friday".
    schedule_text: String,
}

fn routine_view(routine: Routine) -> RoutineView {
    let schedule_text = Schedule::parse(&routine.schedule, &routine.timezone)
        .map(|s| s.describe())
        .unwrap_or_default();
    RoutineView {
        routine,
        schedule_text,
    }
}

fn parse_rid(raw: &str) -> Result<i64, ApiError> {
    raw.parse()
        .map_err(|_| ApiError::NotFound(format!("routine {raw} not found")))
}

fn check_name(name: &str) -> Result<(), ApiError> {
    if name.is_empty() || name.len() > 64 {
        return Err(ApiError::BadRequest("name must be 1-64 bytes".into()));
    }
    Ok(())
}

fn check_prompt(prompt: &str) -> Result<(), ApiError> {
    if prompt.trim().is_empty() {
        return Err(ApiError::BadRequest("prompt must not be empty".into()));
    }
    check_len("prompt", prompt)
}

fn changed(state: &AppState, routine: &Routine) {
    state.scheduler.notify_one();
    state.publish(UiEvent::RoutinesChanged {
        agent_id: routine.agent_id,
    });
}

/// The agent's routines, by name.
#[utoipa::path(get, path = "/v1/agents/{id}/routines", tag = "routines",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = Vec<RoutineView>),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn list_routines(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Vec<RoutineView>> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    Ok(Json(
        state
            .store
            .list_routines(id)
            .await?
            .into_iter()
            .map(routine_view)
            .collect(),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct CreateRoutine {
    /// Unique per agent, 1-64 bytes.
    #[schema(example = "morning-digest")]
    name: String,
    /// 5-field cron expression or alias (`@hourly`, `@daily`, ...).
    #[schema(example = "0 9 * * 1-5")]
    schedule: String,
    /// Message the agent receives on every run.
    prompt: String,
    /// IANA time zone; defaults to the daemon's (see `/v1/health`).
    timezone: Option<String>,
    /// Default true.
    enabled: Option<bool>,
}

/// Creates a routine.
#[utoipa::path(post, path = "/v1/agents/{id}/routines", tag = "routines",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    request_body = CreateRoutine,
    responses((status = 201, body = RoutineView),
        (status = 400, description = "Invalid schedule, time zone or texts", body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody),
        (status = 409, description = "Name already used by this agent", body = ErrorBody)))]
pub(crate) async fn create_routine(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CreateRoutine>,
) -> Result<(StatusCode, Json<RoutineView>), ApiError> {
    let id = parse_id(&id)?;
    let name = req.name.trim().to_owned();
    check_name(&name)?;
    check_prompt(&req.prompt)?;
    let timezone = req
        .timezone
        .unwrap_or_else(|| state.settings.default_timezone.clone());
    let enabled = req.enabled.unwrap_or(true);
    let schedule = req.schedule.trim().to_owned();
    let next_run_at = scheduler::next_run(&schedule, &timezone, enabled)?;
    let routine = state
        .store
        .create_routine(
            id,
            NewRoutine {
                name,
                schedule,
                timezone,
                prompt: req.prompt,
                enabled,
                next_run_at,
            },
        )
        .await?;
    changed(&state, &routine);
    Ok((StatusCode::CREATED, Json(routine_view(routine))))
}

/// Partial update; omitted fields stay unchanged.
#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct UpdateRoutine {
    name: Option<String>,
    schedule: Option<String>,
    prompt: Option<String>,
    timezone: Option<String>,
    enabled: Option<bool>,
}

/// Edits, enables or disables a routine.
#[utoipa::path(patch, path = "/v1/routines/{rid}", tag = "routines",
    params(("rid" = i64, Path, description = "Routine id")),
    request_body = UpdateRoutine,
    responses((status = 200, body = RoutineView),
        (status = 400, body = ErrorBody),
        (status = 404, description = "Unknown routine", body = ErrorBody),
        (status = 409, description = "Name already used by this agent", body = ErrorBody)))]
pub(crate) async fn update_routine(
    State(state): State<AppState>,
    Path(rid): Path<String>,
    Json(req): Json<UpdateRoutine>,
) -> ApiResult<RoutineView> {
    let mut routine = state.store.get_routine(parse_rid(&rid)?).await?;
    if let Some(name) = req.name {
        name.trim().clone_into(&mut routine.name);
        check_name(&routine.name)?;
    }
    if let Some(prompt) = req.prompt {
        check_prompt(&prompt)?;
        routine.prompt = prompt;
    }
    if let Some(schedule) = req.schedule {
        schedule.trim().clone_into(&mut routine.schedule);
    }
    if let Some(tz) = req.timezone {
        routine.timezone = tz;
    }
    if let Some(enabled) = req.enabled {
        routine.enabled = enabled;
    }
    routine.next_run_at =
        scheduler::next_run(&routine.schedule, &routine.timezone, routine.enabled)?;
    let routine = state.store.update_routine(routine).await?;
    changed(&state, &routine);
    Ok(Json(routine_view(routine)))
}

/// Deletes a routine and its run history.
#[utoipa::path(delete, path = "/v1/routines/{rid}", tag = "routines",
    params(("rid" = i64, Path, description = "Routine id")),
    responses((status = 204, description = "Deleted"),
        (status = 404, description = "Unknown routine", body = ErrorBody)))]
pub(crate) async fn delete_routine(
    State(state): State<AppState>,
    Path(rid): Path<String>,
) -> Result<StatusCode, ApiError> {
    let routine = state.store.delete_routine(parse_rid(&rid)?).await?;
    changed(&state, &routine);
    Ok(StatusCode::NO_CONTENT)
}

/// Runs a routine now (does not move its schedule). Wakes the agent if needed.
#[utoipa::path(post, path = "/v1/routines/{rid}/run", tag = "routines",
    params(("rid" = i64, Path, description = "Routine id")),
    responses((status = 202, description = "Prompt queued", body = MailMessage),
        (status = 404, description = "Unknown routine", body = ErrorBody)))]
pub(crate) async fn run_routine(
    State(state): State<AppState>,
    Path(rid): Path<String>,
) -> Result<(StatusCode, Json<MailMessage>), ApiError> {
    let routine = state.store.get_routine(parse_rid(&rid)?).await?;
    let msg = scheduler::fire(&state, &routine, RunTrigger::Manual).await?;
    Ok((StatusCode::ACCEPTED, Json(msg)))
}

/// Run history, newest first (last 50).
#[utoipa::path(get, path = "/v1/routines/{rid}/runs", tag = "routines",
    params(("rid" = i64, Path, description = "Routine id")),
    responses((status = 200, body = Vec<RoutineRun>),
        (status = 404, description = "Unknown routine", body = ErrorBody)))]
pub(crate) async fn list_runs(
    State(state): State<AppState>,
    Path(rid): Path<String>,
) -> ApiResult<Vec<RoutineRun>> {
    let rid = parse_rid(&rid)?;
    state.store.get_routine(rid).await?;
    Ok(Json(state.store.routine_runs(rid, 50).await?))
}
