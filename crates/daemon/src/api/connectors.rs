//! Connectors (remote MCP servers). Tokens go into the daemon's secret store
//! and are never returned, logged or handed to a sandbox.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use std::fmt::Write as _;

use ferrobot_core::question::{
    Approval, QuestionAnswer, QuestionAnswered, QuestionCard, QuestionEvent, approves, take_secret,
};
use ferrobot_core::{AgentId, Connector, ConnectorAuth};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::agents::explicit;
use super::{ApiResult, parse_id};
use crate::connectors::{ConnectorCheck, check, check_soon, secret_key};
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
    /// The authorization server the own OAuth app belongs to.
    issuer: Option<String>,
    /// Register this redirect URI with the provider for an own OAuth app.
    redirect_uri: String,
    /// The last connection check; `null` until one ran since the daemon
    /// started or the connector changed.
    check: Option<ConnectorCheck>,
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
    let check = state.connectors.check(connector.id);
    Ok(ConnectorView {
        connector,
        has_token,
        signed_in: sign_in == oauth::SignIn::SignedIn,
        sign_in,
        client_id: client.client_id,
        has_client_secret: client.client_secret.is_some(),
        scopes: client.scopes,
        issuer: client.issuer,
        redirect_uri: oauth::redirect_uri(state),
        check,
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
            if out.len() == 32 {
                return Err(ApiError::BadRequest("at most 32 scopes".into()));
            }
            out.push(scope.to_owned());
        }
    }
    Ok(out)
}

/// An issuer is an https URL (http only on this machine), no query.
fn clean_issuer(value: Option<String>) -> Result<Option<String>, ApiError> {
    let Some(issuer) = clean_field(value, "issuer", 512)? else {
        return Ok(None);
    };
    let bad = || {
        ApiError::BadRequest("issuer must be an https URL like https://accounts.google.com".into())
    };
    let url = url::Url::parse(&issuer).map_err(|_| bad())?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if !(url.scheme() == "https" || url.scheme() == "http" && local)
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(bad());
    }
    Ok(Some(issuer))
}

/// A client secret needs a client id, and an own app needs its issuer.
fn check_app(client: &mut oauth::ClientConfig) -> Result<(), ApiError> {
    if client.client_id.is_none() {
        if client.client_secret.is_some() {
            return Err(ApiError::BadRequest(
                "a client_secret needs a client_id".into(),
            ));
        }
        client.issuer = None;
    } else if client.issuer.is_none() {
        return Err(ApiError::BadRequest(
            "an own OAuth app needs its issuer (e.g. https://accounts.google.com)".into(),
        ));
    }
    Ok(())
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
    /// Streamable HTTP endpoint of a remote MCP server; leave it out for a local one.
    #[schema(example = "https://api.githubcopilot.com/mcp/")]
    #[serde(default)]
    url: String,
    /// A local MCP server: its program and arguments, started on this computer without a shell
    /// (e.g. `["npx", "-y", "@scope/server"]`).
    #[serde(default)]
    command: Option<Vec<String>>,
    /// The environment variable a local server gets `token` in, e.g. `GITHUB_TOKEN`.
    #[serde(default)]
    token_env: Option<String>,
    /// Sent as `Authorization: Bearer <token>`. Kept in the daemon's secret store;
    /// never returned.
    #[serde(default)]
    token: Option<String>,
    /// Client id of your own OAuth app, for servers without dynamic client
    /// registration (e.g. Google). Register `redirect_uri` with it.
    #[serde(default)]
    client_id: Option<String>,
    /// That app's client secret. Kept in the daemon's secret store; never returned.
    #[serde(default)]
    client_secret: Option<String>,
    /// Scopes to ask for at sign-in (space-separated entries are split).
    #[serde(default)]
    scopes: Vec<String>,
    /// The authorization server the app belongs to (e.g. `https://accounts.google.com`);
    /// needed with `client_id`. Sign-ins through any other server are refused.
    #[serde(default)]
    issuer: Option<String>,
}

/// Adds a connector and checks it (see `POST /v1/connectors/{id}/check`). Agents get its tools
/// once they are assigned it.
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
        command: req.command,
        token_env: req.token_env.map(|e| e.trim().to_owned()),
        created_at: time::OffsetDateTime::now_utc(),
    };
    connector.validate().map_err(ApiError::BadRequest)?;
    let token = clean_token(req.token)?;
    let mut client = oauth::ClientConfig {
        client_id: clean_field(req.client_id, "client_id", 512)?,
        client_secret: clean_field(req.client_secret, "client_secret", 4096)?,
        scopes: clean_scopes(&req.scopes)?,
        issuer: clean_issuer(req.issuer)?,
    };
    check_app(&mut client)?;
    let connector = add(&state, connector, token, &client).await?;
    check(&state, &connector).await;
    Ok((StatusCode::CREATED, Json(view(&state, connector).await?)))
}

/// Stores a validated connector with its token and OAuth app.
async fn add(
    state: &AppState,
    connector: Connector,
    token: Option<String>,
    client: &oauth::ClientConfig,
) -> Result<Connector, ApiError> {
    let connector = state.store.create_connector(connector).await?;
    if let Some(token) = token {
        state
            .settings
            .secrets
            .set(&secret_key(connector.id), &token)
            .await?;
    }
    oauth::set_client_config(state, connector.id, client).await?;
    Ok(connector)
}

/// A command as a shell would show it: arguments with spaces or quotes in single quotes.
fn shown_command(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "@/._-=:+,".contains(c))
            {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Shows the operator a card to add the connector an agent asked for, in the chat they use for
/// it. Nothing exists until they approve; Auto mode does not skip it. No card asks for a key: a
/// token goes in under Settings → Vault.
pub(crate) async fn request_card(
    state: &AppState,
    requester: AgentId,
    request: ferrobot_mcp::ConnectorRequest,
) -> Result<i64, ApiError> {
    let connector = Connector {
        id: Uuid::nil(),
        name: request.name,
        url: request.url,
        command: request.command,
        token_env: request.token_env,
        created_at: time::OffsetDateTime::now_utc(),
    };
    connector.validate().map_err(ApiError::BadRequest)?;
    if state
        .store
        .list_connectors()
        .await?
        .iter()
        .any(|c| c.name == connector.name)
    {
        return Err(ApiError::Conflict(format!(
            "a connector named {} already exists",
            connector.name
        )));
    }
    if !request.reason.is_empty() {
        printable(&request.reason, "reason", 300)?;
    }
    let who = state.store.get_agent(requester).await?.name;
    let Connector {
        name,
        url,
        command,
        token_env,
        ..
    } = connector;
    // A local server runs code on this computer: the card shows exactly what will run.
    let (mut prompt, host) = match &command {
        Some(argv) => (
            format!(
                "Add the local MCP server \"{name}\" and give it to {who}?\nIt runs on this \
                 computer, outside the agents' sandboxes, as: {}",
                shown_command(argv)
            ),
            "the local server".to_owned(),
        ),
        None => (
            format!("Add the connector \"{name}\" and give it to {who}?\nURL: {url}"),
            reqwest::Url::parse(&url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .ok_or_else(|| ApiError::BadRequest("url needs a host".into()))?,
        ),
    };
    if let Some(env) = &token_env {
        let _ = write!(
            prompt,
            "\nIts token goes in as the environment variable {env}."
        );
    }
    match request.auth {
        ConnectorAuth::Token => {
            let _ = write!(
                prompt,
                "\nAfter adding it, put its token in Settings → Vault; it goes only to {host} and \
                 {who} never sees it."
            );
        }
        ConnectorAuth::Oauth => {
            prompt.push_str("\nSign in under Settings → Connectors after adding it.");
        }
        ConnectorAuth::None => {}
    }
    if !request.reason.is_empty() {
        let _ = write!(prompt, "\nWhy: {}", request.reason);
    }
    let card = QuestionEvent::approval(
        format!("{who} wants to add a connector"),
        prompt,
        Approval::AddConnector {
            name,
            url,
            command,
            token_env,
            auth: request.auth,
            requester,
        },
    )
    .in_turn(state.hub.current_turn(requester));
    let chat = crate::agents::chat_of(state, requester).await?;
    crate::agents::post_card(state, chat, &card).await
}

/// The operator's answer to an add-connector card: approved, the connector is created and given to
/// the agent that asked; its token, if any, goes in under Settings → Vault.
pub(crate) async fn answer_card(
    state: &AppState,
    chat: AgentId,
    qid: i64,
    card: &QuestionCard,
    approval: &Approval,
    mut answers: Vec<QuestionAnswer>,
) -> Result<QuestionAnswered, ApiError> {
    let Approval::AddConnector {
        name,
        url,
        command,
        token_env,
        auth,
        requester,
    } = approval
    else {
        return Err(ApiError::Internal("not an add-connector card".into()));
    };
    // Stripped before anything is recorded; only cards from before keys moved to Settings have one.
    let token = clean_token(take_secret(card, &mut answers))?;
    let note = if approves(&answers) {
        let connector = Connector {
            id: Uuid::new_v4(),
            name: name.clone(),
            url: url.clone(),
            command: command.clone(),
            token_env: token_env.clone(),
            created_at: time::OffsetDateTime::now_utc(),
        };
        connector.validate().map_err(ApiError::BadRequest)?;
        let connector = add(state, connector, token, &oauth::ClientConfig::default()).await?;
        check_soon(state, connector.clone());
        if state.store.get_agent(*requester).await.is_ok() {
            let mut ids: Vec<Uuid> = state
                .store
                .agent_connectors(*requester)
                .await?
                .into_iter()
                .map(|c| c.id)
                .collect();
            ids.push(connector.id);
            assign(state, *requester, ids).await?;
        }
        if *auth == ConnectorAuth::Oauth {
            format!(
                "The operator added the connector \"{name}\" ({url}) and gave it to you. Its tools \
                 work once they sign in to it under Settings."
            )
        } else if *auth == ConnectorAuth::Token {
            format!(
                "The operator added the connector \"{name}\" ({url}) and gave it to you. Its tools \
                 work once they put its token in Settings → Vault; never ask for it."
            )
        } else {
            format!(
                "The operator added the connector \"{name}\" ({url}) and gave it to you. Its tools \
                 ({name}__...) work from your next turn."
            )
        }
    } else {
        format!("The operator declined adding the connector \"{name}\".")
    };
    let message_id = if state.store.get_agent(*requester).await.is_ok() {
        Some(crate::agents::tell(state, *requester, note).await?.id)
    } else {
        None
    };
    let answered = QuestionAnswered {
        question: qid,
        answers,
        message_id,
        timed_out: false,
    };
    crate::agents::record_answer(state, chat, answered).await
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
    /// `null` removes the stored client secret. Changing the URL, client id
    /// or issuer without sending a new secret removes it too.
    #[serde(default, deserialize_with = "explicit")]
    #[schema(value_type = Option<String>)]
    client_secret: Option<Option<String>>,
    scopes: Option<Vec<String>>,
    /// Issuer of the own OAuth app.
    #[serde(default, deserialize_with = "explicit")]
    #[schema(value_type = Option<String>)]
    issuer: Option<Option<String>>,
}

/// Edits a connector. Open sessions with it are closed and it is checked again.
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
    if let Some(v) = req.issuer {
        client.issuer = clean_issuer(v)?;
    }
    let moved = c.url != old_url
        || client.client_id != old_client.client_id
        || client.issuer != old_client.issuer;
    // The old app's secret must not go to a new server or app.
    if moved {
        client.client_secret = None;
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
    check_app(&mut client)?;
    state.store.update_connector(c.clone()).await?;
    if client != old_client {
        oauth::set_client_config(&state, id, &client).await?;
    }
    // OAuth tokens belong to the old server or OAuth app.
    if moved {
        oauth::forget_tokens(&state, id).await?;
    }
    if let Some(token) = token {
        super::providers::set_secret(&state, &secret_key(id), token).await?;
    }
    state.connectors.evict_connector(id).await;
    check(&state, &c).await;
    Ok(Json(view(&state, c).await?))
}

/// Deletes a connector, its token and its assignments; OAuth tokens are
/// revoked at the server when it offers that.
#[utoipa::path(delete, path = "/v1/connectors/{id}", tag = "connectors",
    params(("id" = String, Path, description = "Connector id (UUID)")),
    responses((status = 204, description = "Deleted"),
        (status = 404, description = "Unknown connector", body = ErrorBody)))]
pub(crate) async fn delete_connector(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_cid(&id)?;
    let c = state.store.get_connector(id).await?;
    // Secrets first: if the secret store fails, the connector stays to retry.
    oauth::sign_out(&state, &c).await?;
    state.settings.secrets.delete(&secret_key(id)).await?;
    state
        .settings
        .secrets
        .delete(&oauth::client_key(id))
        .await?;
    state.store.delete_connector(id).await?;
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
    Ok(Json(assign(&state, id, ids).await?))
}

/// Replaces an agent's connectors and lets its open sessions and runner pick them up.
async fn assign(state: &AppState, id: AgentId, ids: Vec<Uuid>) -> Result<Vec<Uuid>, ApiError> {
    state.store.set_agent_connectors(id, ids).await?;
    // Codex and Gemini list tools once per process: tell the runner.
    crate::agents::reconfigure(state, id).await?;
    let connectors = state.store.agent_connectors(id).await?;
    Ok(connectors.into_iter().map(|c| c.id).collect())
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct OAuthStart {
    /// Open this in a browser on the daemon's computer to sign in.
    authorization_url: String,
}

/// Starts an OAuth sign-in for a connector without a token and returns the page to sign in on;
/// the browser comes back to `/v1/oauth/callback`.
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
/// when it offers that, then forgets them here either way and checks it again.
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
    check(&state, &c).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Connects to the connector with its current token or sign-in, lists its tools and keeps the
/// outcome as the connector's `check`. Never starts a sign-in. Runs on its own after adding or
/// changing a connector and after a sign-in.
#[utoipa::path(post, path = "/v1/connectors/{id}/check", tag = "connectors",
    params(("id" = String, Path, description = "Connector id (UUID)")),
    responses((status = 200, body = ConnectorCheck),
        (status = 404, description = "Unknown connector", body = ErrorBody)))]
pub(crate) async fn check_connector(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<ConnectorCheck> {
    let c = state.store.get_connector(parse_cid(&id)?).await?;
    Ok(Json(check(&state, &c).await))
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
    if let Ok(id) = outcome
        && let Ok(c) = state.store.get_connector(id).await
    {
        check_soon(&state, c);
    }
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
