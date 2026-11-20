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

/// Request headers the model SDKs need; nothing else is forwarded (e.g. no
/// `cookie`, `openai-organization` or `x-goog-user-project`).
const FORWARD_REQUEST: &[&str] = &[
    "accept",
    "content-type",
    "user-agent",
    "openai-beta",
    "x-request-id",
    "idempotency-key",
    "originator",
    "session_id",
    "conversation_id",
    "version",
    "x-app",
    "x-goog-api-client",
];
const FORWARD_REQUEST_PREFIXES: &[&str] = &["anthropic-", "x-stainless-", "x-amzn-bedrock-"];

/// Response headers copied back to the agent (no `set-cookie` and the like).
const FORWARD_RESPONSE: &[&str] = &[
    "content-type",
    "cache-control",
    "retry-after",
    "date",
    "request-id",
    "x-request-id",
];
const FORWARD_RESPONSE_PREFIXES: &[&str] = &["anthropic-", "openai-", "x-ratelimit-", "x-amzn-"];

fn listed(name: &str, exact: &[&str], prefixes: &[&str]) -> bool {
    exact.contains(&name) || prefixes.iter().any(|p| name.starts_with(p))
}

/// The API family behind a route, for the method and path allowlist.
enum Api {
    Anthropic,
    Bedrock,
    BedrockControl,
    Vertex { project: String, region: String },
    ChatCompletions,
    Responses,
    GeminiNative,
}

/// One path segment: no slash, not `.` or `..`.
fn segment(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && s != "." && s != ".."
}

/// The calls the drivers make, and nothing else: a key's other powers
/// (files, batches, fine-tunes, other projects) stay with the operator.
fn permitted(api: &Api, method: &axum::http::Method, path: &str) -> bool {
    use axum::http::Method;
    let (get, post) = (method == Method::GET, method == Method::POST);
    let action = |rest: Option<&str>, verbs: &[&str]| {
        rest.and_then(|r| r.rsplit_once(':'))
            .is_some_and(|(model, verb)| segment(model) && verbs.contains(&verb))
    };
    match api {
        Api::Anthropic => {
            (post && matches!(path, "v1/messages" | "v1/messages/count_tokens"))
                || (get
                    && (path == "v1/models"
                        || path.strip_prefix("v1/models/").is_some_and(segment)))
        }
        Api::Bedrock => {
            post && path
                .strip_prefix("model/")
                .and_then(|r| r.split_once('/'))
                .is_some_and(|(id, verb)| {
                    segment(id)
                        && matches!(
                            verb,
                            "invoke" | "invoke-with-response-stream" | "count-tokens"
                        )
                })
        }
        Api::BedrockControl => {
            get && (path == "inference-profiles"
                || path
                    .strip_prefix("inference-profiles/")
                    .is_some_and(segment))
        }
        Api::Vertex { project, region } => {
            let models =
                format!("projects/{project}/locations/{region}/publishers/anthropic/models/");
            post && action(
                path.strip_prefix(&models),
                &["rawPredict", "streamRawPredict"],
            )
        }
        Api::ChatCompletions => (post && path == "chat/completions") || (get && path == "models"),
        Api::Responses => {
            (post && matches!(path, "responses" | "responses/compact")) || (get && path == "models")
        }
        Api::GeminiNative => {
            (get && path == "v1beta/models")
                || (post
                    && action(
                        path.strip_prefix("v1beta/models/"),
                        &["generateContent", "streamGenerateContent", "countTokens"],
                    ))
        }
    }
}

/// Plain characters only, so nothing is decoded twice: no `%`, no control
/// characters, no empty or dot segments.
fn clean(path: &str) -> bool {
    path.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-._~:@/".contains(&b))
        && path.split('/').all(segment)
}

/// The final URL stays on the upstream's origin and below its base path.
fn below(base: &str, url: &str) -> bool {
    let (Ok(base), Ok(url)) = (url::Url::parse(base), url::Url::parse(url)) else {
        return false;
    };
    let prefix = format!("{}/", base.path().trim_end_matches('/'));
    url.origin() == base.origin() && url.path().starts_with(&prefix)
}

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
    api: Api,
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
            api: Api::Anthropic,
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
    let api = match provider.kind {
        ProviderKind::Anthropic => Api::Anthropic,
        ProviderKind::Bedrock if path.starts_with("model/") => Api::Bedrock,
        ProviderKind::Bedrock => Api::BedrockControl,
        ProviderKind::Vertex => Api::Vertex {
            project: provider.project.clone().unwrap_or_default(),
            region: provider.region.clone().unwrap_or_default(),
        },
        ProviderKind::Codex => Api::Responses,
        ProviderKind::GeminiCli => Api::GeminiNative,
        ProviderKind::Openai
        | ProviderKind::Gemini
        | ProviderKind::Openrouter
        | ProviderKind::Ollama
        | ProviderKind::OpenaiCompatible => Api::ChatCompletions,
    };
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
            api,
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
        api,
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
    if !clean(&path) {
        tracing::warn!(agent = %agent.name, "model call with an invalid path refused");
        return error(StatusCode::NOT_FOUND, "invalid path");
    }
    let route = match route(&state, &agent, &path).await {
        Ok(route) => route,
        Err((status, message)) => return error(status, &message),
    };
    if !permitted(&route.api, request.method(), &path) {
        // Visible on the host: an SDK update may call an endpoint not listed yet.
        tracing::warn!(agent = %agent.name, method = %request.method(), %path, "model call not proxied");
        return error(StatusCode::NOT_FOUND, "this API call is not proxied");
    }

    let (parts, body) = request.into_parts();
    let Ok(body) = axum::body::to_bytes(body, MAX_REQUEST_BYTES).await else {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
    };
    let mut url = format!("{}/{path}", route.upstream.trim_end_matches('/'));
    if !below(&route.upstream, &url) {
        return error(StatusCode::NOT_FOUND, "invalid path");
    }
    if let Some(q) = parts.uri.query() {
        url.push('?');
        url.push_str(q);
    }
    let mut upstream = state.http.request(parts.method.clone(), &url);
    for (name, value) in &parts.headers {
        if listed(name.as_str(), FORWARD_REQUEST, FORWARD_REQUEST_PREFIXES) {
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
        if listed(name.as_str(), FORWARD_RESPONSE, FORWARD_RESPONSE_PREFIXES) {
            out = out.header(name.clone(), value.clone());
        }
    }
    out.body(Body::from_stream(response.bytes_stream()))
        .unwrap_or_else(|_| error(StatusCode::BAD_GATEWAY, "invalid upstream response"))
}

#[cfg(test)]
mod tests {
    use axum::http::Method;

    use super::*;

    #[test]
    fn paths_stay_plain_and_below_the_base() {
        for bad in [
            "%2e%2e/x", "a/../x", "a/./x", "a//x", "/x", "a\tb", "a b", "a\\b", "a?b", "a#b",
        ] {
            assert!(!clean(bad), "{bad:?}");
        }
        assert!(clean("model/eu.anthropic.claude-x:0/invoke"));
        assert!(clean(
            "projects/p/locations/r/publishers/anthropic/models/m@2025:rawPredict"
        ));
        assert!(below(
            "https://h/base/v1",
            "https://h/base/v1/chat/completions"
        ));
        assert!(!below("https://h/base/v1", "https://h/base/v1x"));
        assert!(!below("https://h/base/v1", "https://h/escape"));
        assert!(!below("https://h/base", "https://other/base/x"));
        assert!(below(
            "https://api.anthropic.com",
            "https://api.anthropic.com/v1/messages"
        ));
    }

    #[test]
    fn only_the_drivers_calls_are_permitted() {
        let (get, post, delete) = (Method::GET, Method::POST, Method::DELETE);
        let ok = |api: &Api, m: &Method, p: &str| permitted(api, m, p);
        assert!(ok(&Api::Anthropic, &post, "v1/messages"));
        assert!(ok(&Api::Anthropic, &get, "v1/models/claude-x"));
        assert!(!ok(&Api::Anthropic, &get, "v1/files"));
        assert!(!ok(&Api::Anthropic, &delete, "v1/messages"));
        assert!(!ok(&Api::Anthropic, &get, "v1/messages/batches/x/results"));
        assert!(ok(&Api::ChatCompletions, &post, "chat/completions"));
        assert!(!ok(&Api::ChatCompletions, &post, "fine_tuning/jobs"));
        assert!(ok(&Api::Responses, &post, "responses"));
        assert!(ok(
            &Api::Bedrock,
            &post,
            "model/eu.anthropic.claude-x/invoke"
        ));
        assert!(!ok(&Api::Bedrock, &post, "model/x/converse/extra"));
        assert!(ok(&Api::BedrockControl, &get, "inference-profiles"));
        assert!(!ok(&Api::BedrockControl, &post, "inference-profiles"));
        let vertex = Api::Vertex {
            project: "p1".into(),
            region: "us-east5".into(),
        };
        let base = "projects/p1/locations/us-east5/publishers/anthropic/models";
        assert!(ok(&vertex, &post, &format!("{base}/m:streamRawPredict")));
        assert!(ok(
            &vertex,
            &post,
            &format!("{base}/count-tokens:rawPredict")
        ));
        assert!(!ok(
            &vertex,
            &post,
            "projects/other/locations/us-east5/publishers/anthropic/models/m:rawPredict"
        ));
        assert!(!ok(
            &vertex,
            &post,
            "projects/p1/locations/us-east5/endpoints/1:predict"
        ));
        assert!(ok(
            &Api::GeminiNative,
            &post,
            "v1beta/models/gemini-2.5-flash:streamGenerateContent"
        ));
        assert!(!ok(&Api::GeminiNative, &post, "v1beta/files"));
    }
}
