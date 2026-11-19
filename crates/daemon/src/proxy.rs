//! Model proxy: sandboxes call the Anthropic API through the daemon.
//!
//! Inside a sandbox `ANTHROPIC_BASE_URL` points at `<agent API>/model` and
//! `ANTHROPIC_API_KEY` holds the agent's own token. The proxy checks that
//! token, swaps in the real key (which never leaves the daemon) and streams
//! the upstream response back unchanged, including server-sent events.

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;

use crate::AppState;

/// Largest request body forwarded (prompts with images can be big).
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;

/// Request headers that must not be forwarded.
const DROP_REQUEST: &[&str] = &[
    "host",
    "authorization",
    "x-api-key",
    "content-length",
    "connection",
    "accept-encoding",
    "transfer-encoding",
    "upgrade",
];

/// Response headers that must not be copied back.
const DROP_RESPONSE: &[&str] = &[
    "content-length",
    "connection",
    "transfer-encoding",
    "content-encoding",
];

pub fn router() -> Router<AppState> {
    Router::new().route("/model/{*path}", any(forward))
}

fn agent_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        })
}

fn error(status: StatusCode, message: &str) -> Response {
    // Same envelope as the Anthropic API, so SDK error messages stay readable.
    let body = serde_json::json!({
        "type": "error",
        "error": { "type": "ferrobot_proxy_error", "message": message }
    });
    (status, axum::Json(body)).into_response()
}

async fn forward(
    State(state): State<AppState>,
    Path(path): Path<String>,
    request: Request,
) -> Response {
    let Some(key) = state.settings.anthropic_api_key.clone() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "the daemon has no ANTHROPIC_API_KEY configured",
        );
    };
    let Some(token) = agent_token(request.headers()) else {
        return error(StatusCode::UNAUTHORIZED, "missing agent token");
    };
    let agent = match state.store.agent_by_token(token).await {
        Ok(Some(agent)) => agent,
        Ok(None) => return error(StatusCode::UNAUTHORIZED, "unknown agent token"),
        Err(e) => {
            tracing::error!(error = %e, "proxy token lookup failed");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "token lookup failed");
        }
    };
    if !path.starts_with("v1/") || path.contains("..") {
        return error(StatusCode::NOT_FOUND, "only /v1/ API paths are proxied");
    }

    let (parts, body) = request.into_parts();
    let Ok(body) = axum::body::to_bytes(body, MAX_REQUEST_BYTES).await else {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
    };
    let mut url = format!(
        "{}/{path}",
        state.settings.anthropic_upstream.trim_end_matches('/')
    );
    if let Some(q) = parts.uri.query() {
        url.push('?');
        url.push_str(q);
    }
    let mut upstream = state.http.request(parts.method.clone(), &url);
    for (name, value) in &parts.headers {
        if !DROP_REQUEST.contains(&name.as_str()) {
            upstream = upstream.header(name, value);
        }
    }
    upstream = upstream.header("x-api-key", key).body(body);

    tracing::debug!(agent = %agent.name, method = %parts.method, %path, "model request");
    let response = match upstream.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(agent = %agent.name, error = %e, "model upstream unreachable");
            return error(StatusCode::BAD_GATEWAY, "model upstream unreachable");
        }
    };
    let status = response.status();
    let mut out = Response::builder().status(status.as_u16());
    for (name, value) in response.headers() {
        if !DROP_RESPONSE.contains(&name.as_str()) {
            out = out.header(name.clone(), value.clone());
        }
    }
    out.body(Body::from_stream(response.bytes_stream()))
        .unwrap_or_else(|_| error(StatusCode::BAD_GATEWAY, "invalid upstream response"))
}
