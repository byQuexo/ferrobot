use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use ferrobot_core::Agent;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::{FerrobotTools, McpState};

/// The authenticated agent behind an MCP request.
#[derive(Debug, Clone)]
pub struct Caller(pub Agent);

/// Extracts the bearer token from an `Authorization` header value.
pub(crate) fn bearer(value: Option<&header::HeaderValue>) -> Option<&str> {
    value?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

async fn authenticate(State(state): State<McpState>, mut req: Request, next: Next) -> Response {
    let Some(token) = bearer(req.headers().get(header::AUTHORIZATION)) else {
        return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
    };
    match state.store.agent_by_token(token).await {
        Ok(Some(agent)) => {
            req.extensions_mut().insert(Caller(agent));
            next.run(req).await
        }
        Ok(None) => (StatusCode::UNAUTHORIZED, "unknown token").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "token lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Axum router serving the MCP endpoint at `/` (nest it, e.g. under `/mcp`).
///
/// Host-header checks are disabled because sandboxes reach the daemon under
/// varying names (`host.docker.internal`, `127.0.0.1`, ...); every request is
/// authenticated with a per-agent bearer token instead.
pub fn router(state: McpState) -> Router {
    let tools_state = state.clone();
    let service = StreamableHttpService::new(
        move || Ok(FerrobotTools::new(tools_state.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().disable_allowed_hosts(),
    );
    Router::new()
        .fallback_service(service)
        .layer(middleware::from_fn_with_state(state, authenticate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bearer() {
        let v = header::HeaderValue::from_static("Bearer abc");
        assert_eq!(bearer(Some(&v)), Some("abc"));
        let v = header::HeaderValue::from_static("Basic abc");
        assert_eq!(bearer(Some(&v)), None);
        let v = header::HeaderValue::from_static("Bearer ");
        assert_eq!(bearer(Some(&v)), None);
        assert_eq!(bearer(None), None);
    }
}
