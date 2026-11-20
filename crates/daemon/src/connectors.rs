//! Connectors: the daemon is the MCP client of each remote server and offers
//! its tools through ferrobot's own MCP server, so tokens never reach a sandbox.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ferrobot_core::{AgentId, Connector};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject, Tool};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use uuid::Uuid;

use crate::AppState;

/// Claude lists tools on every turn, so a dead connector must not stall it.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Connecting includes the MCP handshake and maybe an OAuth refresh.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Claude and `OpenAI` cap function names at 64 chars, including this prefix.
const MAX_TOOL_NAME: usize = 64;
const CLIENT_PREFIX: &str = "mcp__ferrobot__";

type Session = Arc<RunningService<RoleClient, ()>>;

/// Where a connector's token lives in the secret store.
pub fn secret_key(connector: Uuid) -> String {
    format!("connector:{connector}")
}

/// Open upstream sessions, one per agent and connector, so stateful servers
/// never share state between agents.
#[derive(Default)]
pub struct Sessions(tokio::sync::Mutex<HashMap<(AgentId, Uuid), Session>>);

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sessions")
    }
}

impl Sessions {
    /// Drops every agent's session with `connector` (URL or token changed).
    pub async fn evict_connector(&self, connector: Uuid) {
        self.0.lock().await.retain(|(_, c), _| *c != connector);
    }

    /// Drops all of `agent`'s sessions (its connectors changed).
    pub async fn evict_agent(&self, agent: AgentId) {
        self.0.lock().await.retain(|(a, _), _| *a != agent);
    }

    async fn evict(&self, agent: AgentId, connector: Uuid) {
        self.0.lock().await.remove(&(agent, connector));
    }
}

async fn session(state: &AppState, agent: AgentId, c: &Connector) -> Result<Session, String> {
    if let Some(s) = state.connectors.0.lock().await.get(&(agent, c.id))
        && !s.is_closed()
    {
        return Ok(s.clone());
    }
    let token = state
        .settings
        .secrets
        .get(&secret_key(c.id))
        .await
        .map_err(|_| "the secret store is unavailable".to_owned())?;
    let mut config = StreamableHttpClientTransportConfig::with_uri(c.url.as_str());
    let unreachable = |e: &dyn std::fmt::Display| {
        tracing::warn!(connector = %c.name, error = %e, "cannot connect to connector");
        format!("cannot connect to connector {}", c.name)
    };
    let oauth = match token {
        Some(token) => {
            config = config.auth_header(token);
            None
        }
        None => crate::oauth::client(state, c)
            .await
            .map_err(|e| unreachable(&e))?,
    };
    let client = match oauth {
        Some(http) => ().serve(StreamableHttpClientTransport::with_client(http, config)).await,
        None => ().serve(StreamableHttpClientTransport::from_config(config)).await,
    }
    .map_err(|e| unreachable(&e))?;
    let client = Arc::new(client);
    state
        .connectors
        .0
        .lock()
        .await
        .insert((agent, c.id), client.clone());
    Ok(client)
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
}
