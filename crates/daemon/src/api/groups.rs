//! Group chats: named rooms of two or more agents. The operator writes once and the members
//! answer one at a time by the call room's rules; calling the group makes it the call's room.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use ferrobot_core::{AgentId, MailMessage};
use ferrobot_store::Room;
use serde::Deserialize;
use utoipa::ToSchema;

use super::ApiResult;
use super::voice::{CallView, call_text, call_view, load_enabled};
use crate::call::MAX_CALL_AGENTS;
use crate::error::{ApiError, ErrorBody};
use crate::{AppState, UiEvent};

const MAX_NAME_CHARS: usize = 64;

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct CreateGroup {
    /// 1-64 characters.
    name: String,
    /// At least two agents; the first hosts the group's calls.
    members: Vec<AgentId>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct UpdateGroup {
    /// 1-64 characters; omitted keeps the name.
    #[serde(default)]
    name: Option<String>,
    /// At least two agents; omitted keeps them. Those who stay keep their place.
    #[serde(default)]
    members: Option<Vec<AgentId>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct GroupMessage {
    /// 1..19316 bytes after trimming, counting `@name` mentions as their stored `<@agent-id>`
    /// tokens.
    body: String,
}

fn group_name(raw: &str) -> Result<String, ApiError> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(ApiError::BadRequest(format!(
            "name must be 1-{MAX_NAME_CHARS} characters"
        )));
    }
    Ok(name.to_owned())
}

/// Two or more known agents, each once, in the order given.
async fn members(state: &AppState, wanted: Vec<AgentId>) -> Result<Vec<AgentId>, ApiError> {
    let members = super::known_agents(state, wanted).await?;
    if members.len() < 2 {
        return Err(ApiError::BadRequest(
            "a group needs at least two agents".into(),
        ));
    }
    Ok(members)
}

async fn not_in_call(state: &AppState, id: i64) -> Result<(), ApiError> {
    if state.call_view().await.1 == Some(id) {
        return Err(ApiError::Conflict(
            "the group is in a call; hang up first".into(),
        ));
    }
    Ok(())
}

/// Group chats, oldest first.
#[utoipa::path(get, path = "/v1/groups", tag = "groups",
    responses((status = 200, body = Vec<Room>)))]
pub(crate) async fn list_groups(State(state): State<AppState>) -> ApiResult<Vec<Room>> {
    Ok(Json(state.store.groups().await?))
}

/// Creates a group chat. Its transcript is `GET /v1/rooms/{id}`.
#[utoipa::path(post, path = "/v1/groups", tag = "groups", request_body = CreateGroup,
    responses((status = 201, body = Room),
        (status = 400, description = "Bad name or fewer than two agents", body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn create_group(
    State(state): State<AppState>,
    Json(req): Json<CreateGroup>,
) -> Result<(StatusCode, Json<Room>), ApiError> {
    let name = group_name(&req.name)?;
    let members = members(&state, req.members).await?;
    let group = state.store.create_group(name, members).await?;
    state.publish(UiEvent::Room {
        room: group.clone(),
    });
    Ok((StatusCode::CREATED, Json(group)))
}

/// Renames a group or replaces its members; a conversation going on continues with them.
#[utoipa::path(patch, path = "/v1/groups/{id}", tag = "groups",
    params(("id" = i64, Path, description = "Group (room) id")),
    request_body = UpdateGroup,
    responses((status = 200, body = Room),
        (status = 400, description = "Bad name or fewer than two agents", body = ErrorBody),
        (status = 404, description = "Unknown group or agent", body = ErrorBody),
        (status = 409, description = "Members changed while the group is in a call", body = ErrorBody)))]
pub(crate) async fn update_group(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateGroup>,
) -> ApiResult<Room> {
    let mut group = state.store.group(id).await?;
    let name = req.name.as_deref().map(group_name).transpose()?;
    let wanted = match req.members {
        Some(wanted) => {
            not_in_call(&state, id).await?;
            Some(members(&state, wanted).await?)
        }
        None => None,
    };
    if let Some(name) = name {
        group = state.store.rename_group(id, name).await?;
    }
    if let Some(wanted) = wanted {
        group = state.store.set_group_members(id, wanted).await?;
        state.regroup(id, Some(group.members.clone())).await;
    }
    state.publish(UiEvent::Room {
        room: group.clone(),
    });
    Ok(Json(group))
}

/// Deletes a group with its transcript; a member's running turn in it is cut off.
#[utoipa::path(delete, path = "/v1/groups/{id}", tag = "groups",
    params(("id" = i64, Path, description = "Group (room) id")),
    responses((status = 204, description = "Deleted"),
        (status = 404, description = "Unknown group", body = ErrorBody),
        (status = 409, description = "The group is in a call", body = ErrorBody)))]
pub(crate) async fn delete_group(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    state.store.group(id).await?;
    not_in_call(&state, id).await?;
    state.regroup(id, None).await;
    state.store.delete_group(id).await?;
    state.publish(UiEvent::RoomDeleted { room_id: id });
    Ok(StatusCode::NO_CONTENT)
}

/// Sends the operator's message to the group: a line that is due for every member. They answer
/// one at a time in join order, each with what it has not read yet, or pass; at most four agent
/// lines in a row, then the group waits for the operator. Their messages (`room_id` set) stay
/// out of chats. While the group is in a call, the message goes to the call.
#[utoipa::path(post, path = "/v1/groups/{id}/messages", tag = "groups",
    params(("id" = i64, Path, description = "Group (room) id")),
    request_body = GroupMessage,
    responses(
        (status = 202, description = "Queued: the next speaker's message (none while a member \
            still answers)", body = Vec<MailMessage>),
        (status = 400, body = ErrorBody),
        (status = 404, description = "Unknown group", body = ErrorBody)))]
pub(crate) async fn post_group_message(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<GroupMessage>,
) -> Result<(StatusCode, Json<Vec<MailMessage>>), ApiError> {
    let group = state.store.group(id).await?;
    let body = call_text(&state, &req.body).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(state.say_in_group(&group, &body).await),
    ))
}

/// Calls the group: every member joins a voice call whose room is the group, so its lines land
/// there. The first member hosts. End it with `PUT /v1/call` and no agents.
#[utoipa::path(post, path = "/v1/groups/{id}/call", tag = "groups",
    params(("id" = i64, Path, description = "Group (room) id")),
    responses((status = 200, body = CallView),
        (status = 400, description = "More than 4 members", body = ErrorBody),
        (status = 404, description = "Unknown group", body = ErrorBody),
        (status = 409, description = "Voice is switched off, or a call is running", body = ErrorBody)))]
pub(crate) async fn call_group(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<CallView> {
    let mut group = state.store.group(id).await?;
    load_enabled(&state).await?;
    let mut present = Vec::new();
    for agent in group.members {
        if state.store.get_agent(agent).await.is_ok() {
            present.push(agent);
        }
    }
    group.members = present;
    if group.members.is_empty() {
        return Err(ApiError::BadRequest(
            "every agent of the group was deleted".into(),
        ));
    }
    if group.members.len() > MAX_CALL_AGENTS {
        return Err(ApiError::BadRequest(format!(
            "a call has at most {MAX_CALL_AGENTS} agents"
        )));
    }
    state.call_group(&group).await.map_err(ApiError::Conflict)?;
    Ok(Json(call_view(&state).await))
}
