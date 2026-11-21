//! Connectors: the daemon is the MCP client of each remote server and offers
//! its tools through ferrobot's own MCP server, so tokens never reach a sandbox.

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use ferrobot_core::{AgentId, Connector};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject, Tool};
use rmcp::service::{ClientInitializeError, RoleClient, RunningService, ServiceError};
use rmcp::transport::auth::AuthError;
use rmcp::transport::streamable_http_client::{
    AuthRequiredError, InsufficientScopeError, StreamableHttpClientTransportConfig,
    StreamableHttpError,
};
use rmcp::transport::{DynamicTransportError, StreamableHttpClientTransport};
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::AppState;

/// Claude lists tools on every turn, so a dead connector must not stall it.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Connecting includes the MCP handshake and maybe an OAuth refresh.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// A check connects and lists the tools; the UI waits for it after adding a connector.
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// Claude and `OpenAI` cap function names at 64 chars, including this prefix.
const MAX_TOOL_NAME: usize = 64;
const CLIENT_PREFIX: &str = "mcp__ferrobot__";

type Client = RunningService<RoleClient, ()>;
type Session = Arc<Client>;

/// Where a connector's token lives in the secret store.
pub fn secret_key(connector: Uuid) -> String {
    format!("connector:{connector}")
}

/// Open upstream sessions, one per agent and connector, so stateful servers
/// never share state between agents, and the last check of each connector.
#[derive(Default)]
pub struct Sessions {
    open: tokio::sync::Mutex<HashMap<(AgentId, Uuid), Session>>,
    checks: std::sync::Mutex<HashMap<Uuid, ConnectorCheck>>,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sessions")
    }
}

impl Sessions {
    /// Drops every agent's session with `connector` and its last check (URL or token changed).
    pub async fn evict_connector(&self, connector: Uuid) {
        self.open.lock().await.retain(|(_, c), _| *c != connector);
        self.checks_lock().remove(&connector);
    }

    /// Drops all of `agent`'s sessions (its connectors changed).
    pub async fn evict_agent(&self, agent: AgentId) {
        self.open.lock().await.retain(|(a, _), _| *a != agent);
    }

    async fn evict(&self, agent: AgentId, connector: Uuid) {
        self.open.lock().await.remove(&(agent, connector));
    }

    fn checks_lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, ConnectorCheck>> {
        self.checks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The connector's last check since the daemon started or the connector changed.
    pub fn check(&self, connector: Uuid) -> Option<ConnectorCheck> {
        self.checks_lock().get(&connector).cloned()
    }
}

/// What a connection check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Connected and listed its tools.
    Ok,
    /// The server wants credentials and has none: sign in or add a token.
    NeedsSignIn,
    /// Unreachable, not an MCP server, or it rejected the stored token.
    Failed,
}

/// The outcome of connecting to a connector as the daemon and listing its tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct ConnectorCheck {
    pub status: CheckStatus,
    /// Tools agents can use (with `ok`).
    pub tools: Option<usize>,
    /// Why it failed (with `failed`), in ferrobot's words, never the server's.
    pub error: Option<String>,
    /// When the check started.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub at: OffsetDateTime,
}

/// Why connecting failed. `Failed` holds our own text: a server's text may echo the token.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    SignIn,
    Failed(String),
}

fn failed(text: &str) -> Failure {
    Failure::Failed(text.to_owned())
}

/// `reason` is `None` when the server asked for credentials.
fn failure(reason: Option<String>, token: bool) -> Failure {
    match reason {
        Some(text) => Failure::Failed(text),
        None if token => failed("the server rejected the token; replace it in Vault"),
        None => Failure::SignIn,
    }
}

fn auth_reason(e: &AuthError) -> Option<String> {
    match e {
        AuthError::AuthorizationRequired | AuthError::TokenRefreshRejected(_) => None,
        _ => Some("cannot use the OAuth sign-in".into()),
    }
}

fn http_reason(e: &StreamableHttpError<reqwest::Error>) -> Option<String> {
    let status = match e {
        StreamableHttpError::AuthRequired(_) | StreamableHttpError::InsufficientScope(_) => {
            return None;
        }
        StreamableHttpError::Auth(e) => return auth_reason(e),
        StreamableHttpError::Client(e) if e.is_timeout() => {
            return Some("the server did not answer in time".into());
        }
        StreamableHttpError::Client(e) => match e.status() {
            Some(status) => status.as_u16(),
            None => return Some("cannot reach the server".into()),
        },
        StreamableHttpError::Io(_) => return Some("cannot reach the server".into()),
        StreamableHttpError::HttpStatus(e) => e.status,
        // rmcp keeps any other status only in this text: "HTTP 401 Unauthorized: <body>".
        StreamableHttpError::UnexpectedServerResponse(text) => match text
            .strip_prefix("HTTP ")
            .and_then(|t| t.get(..3))
            .and_then(|t| t.parse::<u16>().ok())
        {
            Some(status) => status,
            None => return Some("not an MCP server".into()),
        },
        _ => return Some("not an MCP server".into()),
    };
    match status {
        401 | 403 => None,
        _ => Some(format!("the server answered HTTP {status}")),
    }
}

fn transport_reason(e: &DynamicTransportError) -> Option<String> {
    let mut next: Option<&(dyn Error + 'static)> = Some(e.error.as_ref());
    while let Some(e) = next {
        if e.is::<AuthRequiredError>() || e.is::<InsufficientScopeError>() {
            return None;
        }
        if let Some(e) = e.downcast_ref::<StreamableHttpError<reqwest::Error>>() {
            return http_reason(e);
        }
        if let Some(e) = e.downcast_ref::<AuthError>() {
            return auth_reason(e);
        }
        next = e.source();
    }
    Some("cannot reach the server".into())
}

fn init_reason(e: &ClientInitializeError) -> Option<String> {
    match e {
        ClientInitializeError::TransportError { error, .. } => transport_reason(error),
        ClientInitializeError::LegacyFallbackFailed { fallback, .. } => init_reason(fallback),
        ClientInitializeError::ConnectionClosed(_) => {
            Some("the server closed the connection".into())
        }
        _ => Some("not an MCP server".into()),
    }
}

fn list_reason(e: &ServiceError) -> Option<String> {
    match e {
        ServiceError::TransportSend(error) => transport_reason(error),
        ServiceError::McpError(_) => Some("the server could not list its tools".into()),
        ServiceError::Timeout { .. } => Some("the server did not answer in time".into()),
        _ => Some("the server closed the connection".into()),
    }
}

async fn stored_token(state: &AppState, c: &Connector) -> Result<Option<String>, Failure> {
    state
        .settings
        .secrets
        .get(&secret_key(c.id))
        .await
        .map_err(|_| failed("the secret store is unavailable"))
}

/// A new session with `c` that sends `token`, else its OAuth sign-in if it has one.
async fn connect(
    state: &AppState,
    c: &Connector,
    token: Option<String>,
) -> Result<Client, Failure> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(c.url.as_str());
    let unreachable = |e: &dyn std::fmt::Display| {
        tracing::warn!(connector = %c.name, error = %e, "cannot connect to connector");
    };
    let has_token = token.is_some();
    let oauth = match token {
        Some(token) => {
            config = config.auth_header(token);
            None
        }
        None => crate::oauth::client(state, c).await.map_err(|e| {
            unreachable(&e);
            failure(auth_reason(&e), false)
        })?,
    };
    match oauth {
        Some(http) => ().serve(StreamableHttpClientTransport::with_client(http, config)).await,
        None => ().serve(StreamableHttpClientTransport::from_config(config)).await,
    }
    .map_err(|e| {
        unreachable(&e);
        failure(init_reason(&e), has_token)
    })
}

async fn session(state: &AppState, agent: AgentId, c: &Connector) -> Result<Session, String> {
    if let Some(s) = state.connectors.open.lock().await.get(&(agent, c.id))
        && !s.is_closed()
    {
        return Ok(s.clone());
    }
    let token = stored_token(state, c)
        .await
        .map_err(|_| "the secret store is unavailable".to_owned())?;
    let client = connect(state, c, token)
        .await
        .map_err(|_| format!("cannot connect to connector {}", c.name))?;
    let client = Arc::new(client);
    state
        .connectors
        .open
        .lock()
        .await
        .insert((agent, c.id), client.clone());
    Ok(client)
}

/// Connects to `c` with its current token or sign-in (not as an agent), lists its tools and
/// keeps the outcome until the next check or change.
pub async fn check(state: &AppState, c: &Connector) -> ConnectorCheck {
    let at = OffsetDateTime::now_utc();
    let probe = async {
        let token = stored_token(state, c).await?;
        let has_token = token.is_some();
        let client = connect(state, c, token).await?;
        let tools = client
            .list_all_tools()
            .await
            .map_err(|e| failure(list_reason(&e), has_token))?;
        Ok(tools
            .iter()
            .filter(|t| exposed_name(&c.name, &t.name).is_some())
            .count())
    };
    let outcome = tokio::time::timeout(CHECK_TIMEOUT, probe)
        .await
        .unwrap_or_else(|_| Err(failed("the server did not answer in time")));
    let (status, tools, error) = match outcome {
        Ok(n) => (CheckStatus::Ok, Some(n), None),
        Err(Failure::SignIn) => (CheckStatus::NeedsSignIn, None, None),
        Err(Failure::Failed(e)) => (CheckStatus::Failed, None, Some(e)),
    };
    let check = ConnectorCheck {
        status,
        tools,
        error,
        at,
    };
    let mut checks = state.connectors.checks_lock();
    // An older check that finishes late must not replace a newer one.
    if checks.get(&c.id).is_none_or(|newer| newer.at <= at) {
        checks.insert(c.id, check.clone());
    }
    check
}

/// Runs [`check`] in the background.
pub fn check_soon(state: &AppState, c: Connector) {
    let state = state.clone();
    tokio::spawn(async move { check(&state, &c).await });
}

/// `<connector>__<tool>`, or `None` if a driver could not use the name.
fn exposed_name(connector: &str, tool: &str) -> Option<String> {
    let plain = !tool.is_empty()
        && tool
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    let name = format!("{connector}__{tool}");
    (plain && CLIENT_PREFIX.len() + name.len() <= MAX_TOOL_NAME).then_some(name)
}

async fn list(state: &AppState, agent: AgentId, c: &Connector) -> Result<Vec<Tool>, String> {
    let s = session(state, agent, c).await?;
    let tools = match s.list_all_tools().await {
        Ok(tools) => tools,
        Err(e) => {
            state.connectors.evict(agent, c.id).await;
            return Err(e.to_string());
        }
    };
    Ok(tools
        .into_iter()
        .filter_map(|mut t| {
            let Some(name) = exposed_name(&c.name, &t.name) else {
                tracing::warn!(connector = %c.name, tool = %t.name, "tool name unusable, skipped");
                return None;
            };
            t.name = name.into();
            Some(t)
        })
        .collect())
}

/// The tools of every connector `agent` may use. Connectors that fail or
/// are slow are skipped, so the agent's own tools always work.
pub async fn tools(state: &AppState, agent: AgentId) -> Vec<Tool> {
    let connectors = match state.store.agent_connectors(agent).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "cannot read connectors");
            return Vec::new();
        }
    };
    let lists = futures_util::future::join_all(connectors.iter().map(|c| async move {
        match tokio::time::timeout(LIST_TIMEOUT, list(state, agent, c)).await {
            Ok(Ok(tools)) => tools,
            Ok(Err(e)) => {
                tracing::warn!(connector = %c.name, error = %e, "cannot list connector tools");
                Vec::new()
            }
            Err(_) => {
                state.connectors.evict(agent, c.id).await;
                tracing::warn!(connector = %c.name, "connector did not list its tools in time");
                Vec::new()
            }
        }
    }))
    .await;
    lists.into_iter().flatten().collect()
}

/// Calls `<connector>__<tool>` as `agent`. Never retried: tools have side
/// effects. `Err` is a message for the agent.
pub async fn call(
    state: &AppState,
    agent: AgentId,
    name: &str,
    arguments: Option<JsonObject>,
) -> Result<CallToolResult, String> {
    let unknown = || format!("unknown tool {name}");
    let (connector, tool) = name.split_once("__").ok_or_else(unknown)?;
    let connectors = state.store.agent_connectors(agent).await.map_err(|e| {
        tracing::error!(error = %e, "cannot read connectors");
        "internal storage error".to_owned()
    })?;
    // The assignment is checked here too: an agent can call names it was never shown.
    let c = connectors
        .iter()
        .find(|c| c.name == connector)
        .ok_or_else(unknown)?;
    let s = tokio::time::timeout(CONNECT_TIMEOUT, session(state, agent, c))
        .await
        .map_err(|_| format!("connector {} did not answer in time", c.name))??;
    let mut params = CallToolRequestParams::new(tool.to_owned());
    if let Some(arguments) = arguments {
        params = params.with_arguments(arguments);
    }
    match tokio::time::timeout(CALL_TIMEOUT, s.call_tool(params)).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(ServiceError::McpError(e))) => Err(format!("{}: {}", c.name, e.message)),
        Ok(Err(e)) => {
            state.connectors.evict(agent, c.id).await;
            tracing::warn!(connector = %c.name, error = %e, "connector call failed");
            Err(format!("connector {} failed; try again later", c.name))
        }
        Err(_) => {
            state.connectors.evict(agent, c.id).await;
            Err(format!("connector {} did not answer in time", c.name))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposed_names_fit_every_driver() {
        assert_eq!(
            exposed_name("github", "create_issue").as_deref(),
            Some("github__create_issue")
        );
        assert_eq!(exposed_name("gh", "a.b"), None);
        assert_eq!(exposed_name("gh", ""), None);
        let long = "x".repeat(64 - CLIENT_PREFIX.len() - "gh__".len());
        assert!(exposed_name("gh", &long).is_some());
        assert_eq!(exposed_name("gh", &format!("{long}x")), None);
    }

    #[test]
    fn failures_are_told_in_our_words_and_auth_asks_for_a_sign_in() {
        use rmcp::model::ErrorData;
        use rmcp::transport::streamable_http_client::HttpStatusError;
        type Http = StreamableHttpError<reqwest::Error>;
        const ECHO: &str = "Bearer tok-zebra";
        let transport = |e: Http| ClientInitializeError::TransportError {
            error: DynamicTransportError::from_parts(
                "streamable-http",
                std::any::TypeId::of::<()>(),
                Box::new(e),
            ),
            context: "send initialize request".into(),
        };
        let cases = [
            (
                transport(Http::AuthRequired(AuthRequiredError::new(ECHO.into()))),
                false,
                Failure::SignIn,
            ),
            (
                transport(Http::UnexpectedServerResponse(
                    format!("HTTP 401 Unauthorized: {ECHO}").into(),
                )),
                false,
                Failure::SignIn,
            ),
            (
                transport(Http::UnexpectedServerResponse(
                    format!("HTTP 403 Forbidden: {ECHO}").into(),
                )),
                true,
                failed("the server rejected the token; replace it in Vault"),
            ),
            (
                transport(Http::Auth(AuthError::AuthorizationRequired)),
                false,
                Failure::SignIn,
            ),
            (
                transport(Http::UnexpectedServerResponse(
                    format!("HTTP 404 Not Found: {ECHO}").into(),
                )),
                true,
                failed("the server answered HTTP 404"),
            ),
            (
                transport(Http::HttpStatus(HttpStatusError::new(502, None))),
                false,
                failed("the server answered HTTP 502"),
            ),
            (
                transport(Http::UnexpectedServerResponse(ECHO.into())),
                false,
                failed("not an MCP server"),
            ),
            (
                transport(Http::Auth(AuthError::TokenRefreshFailed(ECHO.into()))),
                false,
                failed("cannot use the OAuth sign-in"),
            ),
            (
                ClientInitializeError::JsonRpcError(ErrorData::internal_error(ECHO, None)),
                true,
                failed("not an MCP server"),
            ),
            (
                ClientInitializeError::ConnectionClosed(ECHO.into()),
                false,
                failed("the server closed the connection"),
            ),
        ];
        for (error, token, want) in cases {
            assert_eq!(failure(init_reason(&error), token), want, "{error}");
        }
        let listed = ServiceError::McpError(ErrorData::internal_error(ECHO, None));
        assert_eq!(
            failure(list_reason(&listed), true),
            failed("the server could not list its tools")
        );
    }
}
