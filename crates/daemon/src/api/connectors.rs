//! Connectors (remote MCP servers). Tokens go into the daemon's secret store
//! and are never returned, logged or handed to a sandbox.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use ferrobot_core::Connector;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::agents::explicit;
use super::{ApiResult, parse_id};
use crate::AppState;
use crate::connectors::secret_key;
use crate::error::{ApiError, ErrorBody};

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ConnectorView {
    #[serde(flatten)]
    connector: Connector,
    /// Whether a token is stored. The token itself is never returned.
    has_token: bool,
}

async fn view(state: &AppState, connector: Connector) -> Result<ConnectorView, ApiError> {
    let has_token = state
        .settings
        .secrets
        .get(&secret_key(connector.id))
        .await?
        .is_some();
    Ok(ConnectorView {
        connector,
        has_token,
    })
}

fn parse_cid(raw: &str) -> Result<Uuid, ApiError> {
    raw.parse()
        .map_err(|_| ApiError::NotFound(format!("connector {raw} not found")))
}

fn clean_token(token: Option<String>) -> Result<Option<String>, ApiError> {
    let token = token.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
    if token
        .as_deref()
        .is_some_and(|t| t.len() > 4096 || t.chars().any(char::is_control))
    {
        return Err(ApiError::BadRequest(
            "token must be 1-4096 printable characters".into(),
        ));
    }
    Ok(token)
}

/// Configured connectors, by name.
#[utoipa::path(get, path = "/v1/connectors", tag = "connectors",
    responses((status = 200, body = Vec<ConnectorView>)))]
pub(crate) async fn list_connectors(
    State(state): State<AppState>,
) -> ApiResult<Vec<ConnectorView>> {
    let mut out = Vec::new();
    for c in state.store.list_connectors().await? {
        out.push(view(&state, c).await?);
    }
    Ok(Json(out))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct CreateConnector {
    /// 1-16 chars, `a-z0-9-`, starts with a letter; prefixes the tools.
    #[schema(example = "github")]
    name: String,
    /// Streamable HTTP endpoint of the MCP server.
    #[schema(example = "https://api.githubcopilot.com/mcp/")]
    url: String,
    /// Sent as `Authorization: Bearer <token>`. Stored in the OS keychain;
    /// never returned.
    #[serde(default)]
    token: Option<String>,
}

/// Adds a connector. Agents get its tools once they are assigned it.
#[utoipa::path(post, path = "/v1/connectors", tag = "connectors",
    request_body = CreateConnector,
    responses((status = 201, body = ConnectorView),
        (status = 400, body = ErrorBody),
        (status = 409, description = "Name already taken", body = ErrorBody)))]
pub(crate) async fn create_connector(
    State(state): State<AppState>,
    Json(req): Json<CreateConnector>,
) -> Result<(StatusCode, Json<ConnectorView>), ApiError> {
    let connector = Connector {
        id: Uuid::new_v4(),
        name: req.name.trim().to_owned(),
        url: req.url.trim().to_owned(),
        created_at: time::OffsetDateTime::now_utc(),
    };
    connector.validate().map_err(ApiError::BadRequest)?;
    let token = clean_token(req.token)?;
    let connector = state.store.create_connector(connector).await?;
    if let Some(token) = token {
        state
            .settings
            .secrets
            .set(&secret_key(connector.id), &token)
            .await?;
    }
    Ok((StatusCode::CREATED, Json(view(&state, connector).await?)))
}

#[derive(Debug, Deserialize, ToSchema)]
#[allow(
    clippy::option_option,
    reason = "omitted vs. explicit null in a PATCH body"
)]
pub(crate) struct UpdateConnector {
    name: Option<String>,
    url: Option<String>,
    /// New token; `null` removes the stored one.
    #[serde(default, deserialize_with = "explicit")]
    #[schema(value_type = Option<String>)]
    token: Option<Option<String>>,
}

/// Edits a connector. Open sessions with it are closed.
#[utoipa::path(patch, path = "/v1/connectors/{id}", tag = "connectors",
    params(("id" = String, Path, description = "Connector id (UUID)")),
    request_body = UpdateConnector,
    responses((status = 200, body = ConnectorView),
        (status = 400, body = ErrorBody),
        (status = 404, description = "Unknown connector", body = ErrorBody),
        (status = 409, description = "Name already taken", body = ErrorBody)))]
pub(crate) async fn update_connector(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateConnector>,
) -> ApiResult<ConnectorView> {
    let id = parse_cid(&id)?;
    let mut c = state.store.get_connector(id).await?;
    if let Some(name) = req.name {
        name.trim().clone_into(&mut c.name);
    }
    if let Some(url) = req.url {
        url.trim().clone_into(&mut c.url);
    }
    c.validate().map_err(ApiError::BadRequest)?;
    let token = req.token.map(clean_token).transpose()?;
    state.store.update_connector(c.clone()).await?;
    match token {
        Some(Some(token)) => state.settings.secrets.set(&secret_key(id), &token).await?,
        Some(None) => state.settings.secrets.delete(&secret_key(id)).await?,
        None => {}
    }
    state.connectors.evict_connector(id).await;
    Ok(Json(view(&state, c).await?))
}

/// Deletes a connector, its token and its assignments.
#[utoipa::path(delete, path = "/v1/connectors/{id}", tag = "connectors",
    params(("id" = String, Path, description = "Connector id (UUID)")),
    responses((status = 204, description = "Deleted"),
        (status = 404, description = "Unknown connector", body = ErrorBody)))]
pub(crate) async fn delete_connector(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_cid(&id)?;
    state.store.delete_connector(id).await?;
    state.settings.secrets.delete(&secret_key(id)).await?;
    state.connectors.evict_connector(id).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Ids of the connectors the agent may use.
#[utoipa::path(get, path = "/v1/agents/{id}/connectors", tag = "connectors",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = Vec<Uuid>),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn agent_connectors(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Vec<Uuid>> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    let connectors = state.store.agent_connectors(id).await?;
    Ok(Json(connectors.into_iter().map(|c| c.id).collect()))
}

/// Replaces the connectors the agent may use. Takes effect on its next turn.
#[utoipa::path(put, path = "/v1/agents/{id}/connectors", tag = "connectors",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    request_body = Vec<Uuid>,
    responses((status = 200, body = Vec<Uuid>),
        (status = 404, description = "Unknown agent or connector", body = ErrorBody)))]
pub(crate) async fn set_agent_connectors(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(ids): Json<Vec<Uuid>>,
) -> ApiResult<Vec<Uuid>> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    state.store.set_agent_connectors(id, ids).await?;
    state.connectors.evict_agent(id).await;
    let connectors = state.store.agent_connectors(id).await?;
    Ok(Json(connectors.into_iter().map(|c| c.id).collect()))
}
