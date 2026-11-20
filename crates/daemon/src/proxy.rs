//! Model proxy: sandboxes call their model provider through the daemon.
//!
//! Inside a sandbox the model base URL points at `<agent API>/model` and the
//! only credential is the agent's own token. The proxy checks that token,
//! picks the upstream from the agent's provider, swaps in the real credential
//! (which never leaves the daemon) and streams the response back unchanged,
//! including server-sent events.

use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;

use ferrobot_core::{Agent, ProviderKind, UpstreamAuth};

use crate::AppState;

/// Google access tokens live an hour; refresh well before.
const GCLOUD_TOKEN_TTL: Duration = Duration::from_secs(45 * 60);

/// Largest request body forwarded (prompts with images can be big).
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;

/// Request headers that must not be forwarded.
const DROP_REQUEST: &[&str] = &[
    "host",
    "authorization",
    "chatgpt-account-id",
    "x-api-key",
    "x-goog-api-key",
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
        .or_else(|| headers.get("x-goog-api-key"))
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

/// Where one agent's model requests go and with which credential.
struct Route {
    upstream: String,
    auth: UpstreamAuth,
    secret: Option<String>,
    /// `ChatGPT` account of a Codex provider signed in with `ChatGPT`.
    chatgpt_account: Option<String>,
}

pub fn secret_key(provider: uuid::Uuid) -> String {
    format!("provider:{provider}")
}

/// Status and message for the agent when a request cannot be routed.
type Failure = (StatusCode, String);

async fn route(state: &AppState, agent: &Agent, path: &str) -> Result<Route, Failure> {
    let Some(id) = agent.provider_id else {
        if !path.starts_with("v1/") {
            return Err((
                StatusCode::NOT_FOUND,
                "only /v1/ API paths are proxied".into(),
            ));
        }
        let Some(key) = state.settings.anthropic_api_key.clone() else {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "the daemon has no ANTHROPIC_API_KEY configured".into(),
            ));
        };
        return Ok(Route {
            upstream: state.settings.anthropic_upstream.clone(),
            auth: UpstreamAuth::ApiKeyHeader,
            secret: Some(key),
            chatgpt_account: None,
        });
    };
    let unavailable = |msg: String| (StatusCode::SERVICE_UNAVAILABLE, msg);
    let provider = state
        .store
        .get_provider(id)
        .await
        .map_err(|e| unavailable(format!("provider: {e}")))?;
    if provider.kind == ProviderKind::Anthropic && !path.starts_with("v1/") {
        return Err((
            StatusCode::NOT_FOUND,
            "only /v1/ API paths are proxied".into(),
        ));
    }
    // Bedrock model listings live on the control plane, invocations on the runtime.
    let upstream = if provider.kind == ProviderKind::Bedrock && !path.starts_with("model/") {
        provider
            .region
            .as_ref()
            .map(|r| format!("https://bedrock.{r}.amazonaws.com"))
    } else {
        provider.upstream()
    };
    let upstream = upstream
        .ok_or_else(|| unavailable(format!("provider {} has no upstream", provider.name)))?;
    let auth = provider.kind.upstream_auth();
    let secret = match auth {
        UpstreamAuth::GcloudToken => Some(gcloud_token(state).await?),
        _ => state
            .settings
            .secrets
            .get(&secret_key(id))
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    };
    if secret.is_none() && provider.kind == ProviderKind::Codex {
        let (token, account) = crate::codex::chatgpt_token(state, id)
            .await
            .map_err(unavailable)?;
        return Ok(Route {
            upstream: state.settings.chatgpt_upstream.clone(),
            auth: UpstreamAuth::Bearer,
            secret: Some(token),
            chatgpt_account: Some(account),
        });
    }
    if secret.is_none() && provider.kind.needs_key() {
        return Err(unavailable(format!(
            "provider {} has no API key; set one in the providers settings",
            provider.name
        )));
    }
    Ok(Route {
        upstream,
        auth,
        secret,
        chatgpt_account: None,
    })
}

/// A cached access token and when it was minted; `Debug` never shows it.
#[derive(Default)]
pub struct CachedToken(pub(crate) Option<(String, Instant)>);

impl std::fmt::Debug for CachedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "CachedToken(..)"
        } else {
            "CachedToken(None)"
        })
    }
}

/// A Google access token from the daemon host's `gcloud` login, cached.
async fn gcloud_token(state: &AppState) -> Result<String, Failure> {
    let mut cached = state.gcloud_token.lock().await;
    if let Some((token, minted)) = cached.0.as_ref()
        && minted.elapsed() < GCLOUD_TOKEN_TTL
    {
        return Ok(token.clone());
    }
    let failed = || {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "could not get a Google access token; run `gcloud auth login` on the daemon host"
                .into(),
        )
    };
    let [program, args @ ..] = state.settings.gcloud_token_cmd.as_slice() else {
        return Err(failed());
    };
    let output = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "gcloud token command failed to start");
            failed()
        })?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() || token.is_empty() {
        tracing::warn!(status = %output.status, "gcloud token command failed");
        return Err(failed());
    }
    cached.0 = Some((token.clone(), Instant::now()));
    Ok(token)
}

async fn forward(
    State(state): State<AppState>,
    Path(path): Path<String>,
    request: Request,
) -> Response {
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
    if path.contains("..") {
        return error(StatusCode::NOT_FOUND, "invalid path");
    }
    let route = match route(&state, &agent, &path).await {
        Ok(route) => route,
        Err((status, message)) => return error(status, &message),
    };

    let (parts, body) = request.into_parts();
    let Ok(body) = axum::body::to_bytes(body, MAX_REQUEST_BYTES).await else {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
    };
    let mut url = format!("{}/{path}", route.upstream.trim_end_matches('/'));
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
    if let Some(secret) = route.secret {
        upstream = match route.auth {
            UpstreamAuth::ApiKeyHeader => upstream.header("x-api-key", secret),
            UpstreamAuth::GoogApiKey => upstream.header("x-goog-api-key", secret),
            UpstreamAuth::Bearer | UpstreamAuth::GcloudToken => {
                upstream.header(header::AUTHORIZATION, format!("Bearer {secret}"))
            }
        };
    }
    if let Some(account) = route.chatgpt_account {
        upstream = upstream.header("chatgpt-account-id", account);
    }
    upstream = upstream.body(body);

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
