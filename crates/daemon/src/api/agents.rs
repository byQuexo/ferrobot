//! Agent registry and profiles.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use ferrobot_core::AgentName;
use ferrobot_core::protocol::DaemonFrame;
use ferrobot_store::{AgentPatch, Avatar, NewAgent};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

use super::{ApiResult, check_len, parse_id};
use crate::error::{ApiError, ErrorBody};
use crate::views::{self, AgentView};
use crate::{AppState, lifecycle};

/// Largest accepted avatar image.
const MAX_AVATAR_BYTES: usize = 512 * 1024;
const AVATAR_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];
/// Upper bound for the one-line description.
const MAX_DESCRIPTION_BYTES: usize = 280;

/// Lists all agents.
#[utoipa::path(get, path = "/v1/agents", tag = "agents",
    responses((status = 200, body = Vec<AgentView>)))]
pub(crate) async fn list_agents(State(state): State<AppState>) -> ApiResult<Vec<AgentView>> {
    Ok(Json(views::all_agents(&state).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct CreateAgent {
    /// 1-32 chars, `a-z0-9-`, starts with a letter. Cannot be changed later.
    #[schema(example = "alice")]
    name: String,
    /// One line shown in agent lists (max 280 bytes).
    #[serde(default)]
    description: String,
    /// System instructions (role) for the agent's model.
    #[serde(default)]
    instructions: String,
}

fn check_description(d: &str) -> Result<(), ApiError> {
    if d.len() > MAX_DESCRIPTION_BYTES {
        return Err(ApiError::BadRequest(format!(
            "description exceeds {MAX_DESCRIPTION_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Registers an agent and provisions its sandbox.
#[utoipa::path(post, path = "/v1/agents", tag = "agents", request_body = CreateAgent,
    responses(
        (status = 201, body = AgentView),
        (status = 400, description = "Invalid name or texts", body = ErrorBody),
        (status = 409, description = "Name already taken", body = ErrorBody),
        (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn create_agent(
    State(state): State<AppState>,
    Json(req): Json<CreateAgent>,
) -> Result<(StatusCode, Json<AgentView>), ApiError> {
    let name = AgentName::parse(req.name.trim())?;
    let description = req.description.trim().to_owned();
    check_description(&description)?;
    check_len("instructions", &req.instructions)?;
    let agent = state
        .store
        .create_agent(NewAgent {
            name,
            description,
            instructions: req.instructions,
        })
        .await?;
    lifecycle::provision(&state, &agent).await?;
    Ok((
        StatusCode::CREATED,
        Json(views::agent_view(&state, agent.id).await?),
    ))
}

/// Fetches one agent.
#[utoipa::path(get, path = "/v1/agents/{id}", tag = "agents",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = AgentView),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn get_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<AgentView> {
    Ok(Json(views::agent_view(&state, parse_id(&id)?).await?))
}

/// Partial profile update. Omitted fields stay unchanged.
#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct UpdateAgent {
    description: Option<String>,
    /// New system instructions; a connected runner applies them from its next turn.
    instructions: Option<String>,
}

/// Edits an agent's profile.
#[utoipa::path(patch, path = "/v1/agents/{id}", tag = "agents",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    request_body = UpdateAgent,
    responses((status = 200, body = AgentView),
        (status = 400, body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn update_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateAgent>,
) -> ApiResult<AgentView> {
    let id = parse_id(&id)?;
    let description = req.description.map(|d| d.trim().to_owned());
    if let Some(d) = &description {
        check_description(d)?;
    }
    if let Some(i) = &req.instructions {
        check_len("instructions", i)?;
    }
    state
        .store
        .update_agent_profile(
            id,
            AgentPatch {
                description,
                instructions: req.instructions.clone(),
            },
        )
        .await?;
    if let Some(instructions) = req.instructions {
        state.hub.send(id, DaemonFrame::Configure { instructions });
    }
    state.publish_agent(id).await;
    Ok(Json(views::agent_view(&state, id).await?))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct DeleteQuery {
    /// Also delete the agent's persistent disk and snapshots.
    #[serde(default)]
    purge: bool,
}

/// Deletes an agent and its sandbox.
#[utoipa::path(delete, path = "/v1/agents/{id}", tag = "agents",
    params(("id" = String, Path, description = "Agent id (UUID)"), DeleteQuery),
    responses((status = 204, description = "Deleted"),
        (status = 404, description = "Unknown agent", body = ErrorBody),
        (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    lifecycle::delete(&state, parse_id(&id)?, q.purge).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The agent's avatar image.
#[utoipa::path(get, path = "/v1/agents/{id}/avatar", tag = "agents",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses(
        (status = 200, description = "The image", content_type = "image/*", body = Vec<u8>),
        (status = 404, description = "Unknown agent or no avatar", body = ErrorBody)))]
pub(crate) async fn get_avatar(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let avatar = state
        .store
        .get_avatar(parse_id(&id)?)
        .await?
        .ok_or_else(|| ApiError::NotFound("no avatar".into()))?;
    Ok((
        [
            (header::CONTENT_TYPE, avatar.mime),
            (header::CACHE_CONTROL, "private, max-age=31536000".into()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
        ],
        avatar.bytes,
    )
        .into_response())
}

/// Sniffs the image type from magic bytes (the declared type must match).
fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

/// Uploads (replaces) the avatar. Body: the raw image.
#[utoipa::path(put, path = "/v1/agents/{id}/avatar", tag = "agents",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    request_body(content = Vec<u8>, content_type = "image/*",
        description = "PNG, JPEG, WebP or GIF, at most 512 KiB"),
    responses((status = 200, body = AgentView),
        (status = 400, description = "Not a supported image", body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn put_avatar(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<AgentView> {
    let id = parse_id(&id)?;
    if body.len() > MAX_AVATAR_BYTES {
        return Err(ApiError::BadRequest("avatar exceeds 512 KiB".into()));
    }
    let declared = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let sniffed = sniff_image(&body)
        .ok_or_else(|| ApiError::BadRequest("avatar must be PNG, JPEG, WebP or GIF".into()))?;
    if !AVATAR_TYPES.contains(&declared) || declared != sniffed {
        return Err(ApiError::BadRequest(format!(
            "content type {declared:?} does not match the image ({sniffed})"
        )));
    }
    state
        .store
        .set_avatar(
            id,
            Some(Avatar {
                mime: sniffed.to_owned(),
                bytes: body.to_vec(),
            }),
        )
        .await?;
    state.publish_agent(id).await;
    Ok(Json(views::agent_view(&state, id).await?))
}

/// Removes the avatar (clients fall back to initials).
#[utoipa::path(delete, path = "/v1/agents/{id}/avatar", tag = "agents",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = AgentView),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn delete_avatar(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<AgentView> {
    let id = parse_id(&id)?;
    state.store.set_avatar(id, None).await?;
    state.publish_agent(id).await;
    Ok(Json(views::agent_view(&state, id).await?))
}

#[cfg(test)]
mod tests {
    use super::sniff_image;

    #[test]
    fn sniffs_supported_formats() {
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff_image(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"GIF89a.."), Some("image/gif"));
        assert_eq!(sniff_image(b"<svg>"), None);
    }
}
