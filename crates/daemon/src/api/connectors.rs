//! Connectors (remote MCP servers). Tokens go into the daemon's secret store
//! and are never returned, logged or handed to a sandbox.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use ferrobot_core::Connector;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::agents::explicit;
use super::{ApiResult, parse_id};
use crate::connectors::secret_key;
use crate::error::{ApiError, ErrorBody};
use crate::{AppState, oauth};

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ConnectorView {
    #[serde(flatten)]
    connector: Connector,
    /// Whether a token is stored. The token itself is never returned.
    has_token: bool,
    /// Whether the connector is signed in with OAuth.
    signed_in: bool,
    sign_in: oauth::SignIn,
    /// Client id of the operator's own OAuth app, if set (else dynamic
    /// client registration is used).
    client_id: Option<String>,
    /// Whether that app's client secret is stored. Never returned.
    has_client_secret: bool,
    /// Scopes asked for at sign-in; empty: the server's choice.
    scopes: Vec<String>,
    /// Register this redirect URI with the provider for an own OAuth app.
    redirect_uri: String,
}

async fn view(state: &AppState, connector: Connector) -> Result<ConnectorView, ApiError> {
    let has_token = state
        .settings
        .secrets
        .get(&secret_key(connector.id))
        .await?
        .is_some();
    let sign_in = oauth::sign_in(state, connector.id).await;
    let client = oauth::client_config(state, connector.id)
        .await
        .map_err(|e| ApiError::Internal(format!("secret store: {e}")))?;
    Ok(ConnectorView {
        connector,
        has_token,
        signed_in: sign_in == oauth::SignIn::SignedIn,
        sign_in,
        client_id: client.client_id,
        has_client_secret: client.client_secret.is_some(),
        scopes: client.scopes,
        redirect_uri: oauth::redirect_uri(state),
    })
}

fn printable(value: &str, what: &str, max: usize) -> Result<(), ApiError> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(format!(
            "{what} must be 1-{max} printable characters"
        )));
    }
    Ok(())
}

fn clean_scopes(scopes: &[String]) -> Result<Vec<String>, ApiError> {
    let mut out: Vec<String> = Vec::new();
    for scope in scopes.iter().flat_map(|s| s.split_whitespace()) {
        printable(scope, "a scope", 256)?;
        if !out.iter().any(|s| s == scope) {
            out.push(scope.to_owned());
        }
    }
    if out.len() > 32 {
        return Err(ApiError::BadRequest("at most 32 scopes".into()));
    }
    Ok(out)
}

/// Trimmed, `None` when empty, checked.
fn clean_field(value: Option<String>, what: &str, max: usize) -> Result<Option<String>, ApiError> {
    let value = value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    if let Some(v) = &value {
        printable(v, what, max)?;
        if v.contains(char::is_whitespace) && what == "client_id" {
            return Err(ApiError::BadRequest("client_id has no spaces".into()));
        }
    }
    Ok(value)
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
    /// Client id of your own OAuth app, for servers without dynamic client
    /// registration (e.g. Google). Register `redirect_uri` with it.
    #[serde(default)]
    client_id: Option<String>,
    /// That app's client secret. Stored in the OS keychain; never returned.
    #[serde(default)]
    client_secret: Option<String>,
    /// Scopes to ask for at sign-in (space-separated entries are split).
    #[serde(default)]
    scopes: Vec<String>,
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
    let client = oauth::ClientConfig {
        client_id: clean_field(req.client_id, "client_id", 512)?,
        client_secret: clean_field(req.client_secret, "client_secret", 4096)?,
        scopes: clean_scopes(&req.scopes)?,
    };
    if client.client_secret.is_some() && client.client_id.is_none() {
        return Err(ApiError::BadRequest(
            "a client_secret needs a client_id".into(),
        ));
    }
    let connector = state.store.create_connector(connector).await?;
    if let Some(token) = token {
        state
            .settings
            .secrets
            .set(&secret_key(connector.id), &token)
            .await?;
    }
    oauth::set_client_config(&state, connector.id, &client).await?;
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
    /// Own OAuth app; `null` goes back to dynamic client registration.
    /// Changing the app signs the connector out.
    #[serde(default, deserialize_with = "explicit")]
    #[schema(value_type = Option<String>)]
    client_id: Option<Option<String>>,
    /// `null` removes the stored client secret.
    #[serde(default, deserialize_with = "explicit")]
    #[schema(value_type = Option<String>)]
    client_secret: Option<Option<String>>,
    scopes: Option<Vec<String>>,
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
    let old_url = c.url.clone();
    if let Some(name) = req.name {
        name.trim().clone_into(&mut c.name);
    }
    if let Some(url) = req.url {
        url.trim().clone_into(&mut c.url);
    }
    c.validate().map_err(ApiError::BadRequest)?;
    let token = req.token.map(clean_token).transpose()?;
    let old_client = oauth::client_config(&state, id)
        .await
        .map_err(|e| ApiError::Internal(format!("secret store: {e}")))?;
    let mut client = old_client.clone();
    if let Some(v) = req.client_id {
        client.client_id = clean_field(v, "client_id", 512)?;
    }
    if let Some(v) = req.client_secret {
        client.client_secret = clean_field(v, "client_secret", 4096)?;
    }
    if client.client_id.is_none() {
        client.client_secret = None;
    }
    if let Some(scopes) = req.scopes {
        client.scopes = clean_scopes(&scopes)?;
    }
    state.store.update_connector(c.clone()).await?;
    if client != old_client {
        oauth::set_client_config(&state, id, &client).await?;
    }
    // OAuth tokens belong to the old server or OAuth app.
    if c.url != old_url || client.client_id != old_client.client_id {
        // OAuth tokens belong to the old server.
        state
            .settings
            .secrets
            .delete(&oauth::secret_key(id))
            .await?;
        state.oauth.forget(id).await;
    }
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
    state
        .settings
        .secrets
        .delete(&oauth::client_key(id))
        .await?;
    state
        .settings
        .secrets
        .delete(&oauth::secret_key(id))
        .await?;
    state.oauth.forget(id).await;
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

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct OAuthStart {
    /// Open this in a browser on the daemon's computer to sign in.
    authorization_url: String,
}

/// Starts an OAuth sign-in for a connector without a token: discovers the
/// server's authorization server, registers ferrobot as a client and returns
/// the page to sign in on. The browser comes back to `/v1/oauth/callback`.
#[utoipa::path(post, path = "/v1/connectors/{id}/oauth", tag = "connectors",
    params(("id" = String, Path, description = "Connector id (UUID)")),
    responses((status = 200, body = OAuthStart),
        (status = 400, description = "Uses a token, or the server offers no usable OAuth", body = ErrorBody),
        (status = 404, description = "Unknown connector", body = ErrorBody)))]
pub(crate) async fn start_oauth(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<OAuthStart> {
    let c = state.store.get_connector(parse_cid(&id)?).await?;
    if state
        .settings
        .secrets
        .get(&secret_key(c.id))
        .await?
        .is_some()
    {
        return Err(ApiError::BadRequest(format!(
            "{} uses a token; remove it to sign in with OAuth",
            c.name
        )));
    }
    let authorization_url = oauth::start(&state, &c)
        .await
        .map_err(ApiError::BadRequest)?;
    Ok(Json(OAuthStart { authorization_url }))
}

/// Signs a connector out: asks the server to revoke its tokens (RFC 7009)
/// when it offers that, then forgets them here either way.
#[utoipa::path(delete, path = "/v1/connectors/{id}/oauth", tag = "connectors",
    params(("id" = String, Path, description = "Connector id (UUID)")),
    responses((status = 204, description = "Signed out"),
        (status = 404, description = "Unknown connector", body = ErrorBody)))]
pub(crate) async fn sign_out(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let c = state.store.get_connector(parse_cid(&id)?).await?;
    oauth::sign_out(&state, &c).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct OAuthCallback {
    state: Option<String>,
    code: Option<String>,
    /// RFC 9207 issuer, checked when present.
    iss: Option<String>,
}

/// Where the authorization server sends the browser back. Needs no operator
/// token: the single-use `state` from the sign-in it started authorizes it.
#[utoipa::path(get, path = "/v1/oauth/callback", tag = "connectors",
    params(OAuthCallback),
    responses((status = 200, description = "Signed in (HTML page)", content_type = "text/html", body = String),
        (status = 400, description = "Unknown, used, expired or failed sign-in (HTML page)", content_type = "text/html", body = String)))]
pub(crate) async fn oauth_callback(
    State(state): State<AppState>,
    Query(q): Query<OAuthCallback>,
) -> Response {
    let outcome = match q.state.as_deref() {
        Some(csrf) => oauth::finish(&state, csrf, q.code.as_deref(), q.iss.as_deref()).await,
        None => Err("This page needs a sign-in to finish."),
    };
    let (status, text) = match outcome {
        Ok(_) => (
            StatusCode::OK,
            "Signed in. You can close this tab and go back to ferrobot.",
        ),
        Err(reason) => (StatusCode::BAD_REQUEST, reason),
    };
    // Static text only: nothing from the request is echoed.
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>ferrobot</title>\
         <body style=\"font:16px system-ui;margin:3em\"><p>{text}</p>"
    );
    (
        status,
        [
            (header::CACHE_CONTROL, "no-store"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'",
            ),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        Html(page),
    )
        .into_response()
}
