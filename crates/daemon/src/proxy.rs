//! Model proxy at `<agent API>/model`: checks the agent token, picks the upstream from the agent's
//! provider and swaps in the real credential, which never leaves the daemon; each call leaves a usage row.

use std::task::Poll;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use futures_util::StreamExt;

use ferrobot_core::{Provider, ProviderKind, UpstreamAuth};
use ferrobot_store::{Tokens, UsageStatus};

use crate::usage::{Call, Family, Meter};
use crate::views::CredentialProblem;
use crate::{AppState, credentials, no_tools};

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

/// The usage shape of a permitted call that runs the model; `None` for
/// model lists and token counts.
fn generation(api: &Api, path: &str) -> Option<Family> {
    let verb = |verbs: &[&str]| {
        path.rsplit_once(':')
            .is_some_and(|(_, v)| verbs.contains(&v))
    };
    match api {
        Api::Anthropic => (path == "v1/messages").then_some(Family::Anthropic),
        Api::Bedrock => (!path.ends_with("/count-tokens")).then_some(Family::Anthropic),
        Api::BedrockControl => None,
        Api::Vertex { .. } => (verb(&["rawPredict", "streamRawPredict"])
            && !path.contains("/count-tokens:"))
        .then_some(Family::Anthropic),
        Api::ChatCompletions => (path == "chat/completions").then_some(Family::ChatCompletions),
        Api::Responses => path.starts_with("responses").then_some(Family::Responses),
        Api::GeminiNative => {
            verb(&["generateContent", "streamGenerateContent"]).then_some(Family::Gemini)
        }
    }
}

/// Bedrock, Vertex and Gemini run (and bill) the model in the path, whatever the body says.
fn body_names_model(api: &Api) -> bool {
    !matches!(api, Api::Bedrock | Api::Vertex { .. } | Api::GeminiNative)
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

pub(crate) fn agent_token(headers: &HeaderMap) -> Option<&str> {
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
    /// `OpenRouter` gets caching and sticky routing added (see `openrouter_extras`).
    openrouter: bool,
    /// The agent's status reason when the upstream refuses the credential.
    rejected: String,
}

/// For `OpenRouter`: one session per agent (sticky routing keeps its calls on
/// the provider holding its cache) and caching for Claude chat calls.
fn openrouter_extras(
    upstream: reqwest::RequestBuilder,
    route: &Route,
    agent: ferrobot_core::AgentId,
    body: axum::body::Bytes,
) -> (reqwest::RequestBuilder, axum::body::Bytes) {
    if !route.openrouter {
        return (upstream, body);
    }
    let upstream = upstream.header("x-session-id", agent.to_string());
    match route.api {
        Api::ChatCompletions => (upstream, with_openrouter_cache(body)),
        _ => (upstream, body),
    }
}

/// The request with `model` in place of the one it names.
fn with_model(body: axum::body::Bytes, model: &str) -> axum::body::Bytes {
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    match json.as_object_mut() {
        Some(obj) if obj.contains_key("model") => {
            obj.insert("model".into(), model.into());
            serde_json::to_vec(&json).map_or(body, axum::body::Bytes::from)
        }
        _ => body,
    }
}

/// `OpenRouter` caches Claude only when asked; one top-level `cache_control`
/// turns on automatic caching. Other models there cache on their own.
fn with_openrouter_cache(body: axum::body::Bytes) -> axum::body::Bytes {
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let claude = json["model"]
        .as_str()
        .is_some_and(|m| m.starts_with("anthropic/"));
    match json.as_object_mut() {
        Some(obj) if claude && !obj.contains_key("cache_control") => {
            obj.insert(
                "cache_control".into(),
                serde_json::json!({ "type": "ephemeral" }),
            );
            serde_json::to_vec(&json).map_or(body, axum::body::Bytes::from)
        }
        _ => body,
    }
}

/// Where a provider's API key lives in the secret store.
pub fn secret_key(provider: uuid::Uuid) -> String {
    format!("provider:{provider}")
}

/// Status and message for the agent when a request cannot be routed.
struct Failure {
    status: StatusCode,
    message: String,
    /// The credential is missing: the agent's turns wait for the operator (see `credentials`).
    missing_key: bool,
}

impl Failure {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            missing_key: false,
        }
    }
}

/// A setup problem only the operator can fix: a client error, so SDKs fail at once instead of
/// retrying for minutes as they do on 5xx.
fn unconfigured(message: String) -> Failure {
    Failure::new(StatusCode::BAD_REQUEST, message)
}

fn missing_key(message: String) -> Failure {
    Failure {
        missing_key: true,
        ..unconfigured(message)
    }
}

/// An agent without a provider: Anthropic with the daemon's key.
fn daemon_route(state: &AppState, path: &str) -> Result<Route, Failure> {
    if !path.starts_with("v1/") {
        return Err(Failure::new(
            StatusCode::NOT_FOUND,
            "only /v1/ API paths are proxied",
        ));
    }
    let Some(key) = state.settings.anthropic_api_key.clone() else {
        return Err(missing_key(crate::agents::NO_DAEMON_KEY.into()));
    };
    Ok(Route {
        api: Api::Anthropic,
        upstream: state.settings.anthropic_upstream.clone(),
        auth: UpstreamAuth::ApiKeyHeader,
        secret: Some(key),
        chatgpt_account: None,
        openrouter: false,
        rejected: "Anthropic rejected the daemon's key: pick a model provider for this agent"
            .into(),
    })
}

async fn route(state: &AppState, provider: Option<Provider>, path: &str) -> Result<Route, Failure> {
    let Some(provider) = provider else {
        return daemon_route(state, path);
    };
    let id = provider.id;
    let unavailable = unconfigured;
    if provider.kind == ProviderKind::Anthropic && !path.starts_with("v1/") {
        return Err(Failure::new(
            StatusCode::NOT_FOUND,
            "only /v1/ API paths are proxied",
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
        ProviderKind::ClaudeCode => {
            let name = &provider.name;
            return Err(unavailable(format!(
                "provider {name} runs on the host's claude CLI"
            )));
        }
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
            .map_err(|e| Failure::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    };
    let name = &provider.name;
    if secret.is_none() && provider.kind == ProviderKind::Codex {
        let (token, account) = crate::codex::chatgpt_token(state, id).await.map_err(|e| {
            tracing::warn!(provider = %name, error = %e, "no ChatGPT token");
            missing_key(format!(
                "provider {name} could not use its ChatGPT sign-in: sign in again in \
                 Settings, Model providers, or add an API key in Vault"
            ))
        })?;
        return Ok(Route {
            api,
            upstream: state.settings.chatgpt_upstream.clone(),
            auth: UpstreamAuth::Bearer,
            secret: Some(token),
            chatgpt_account: Some(account),
            openrouter: false,
            rejected: format!(
                "ChatGPT rejected the sign-in of provider {name}: sign in again in Settings, \
                 Model providers"
            ),
        });
    }
    if secret.is_none() && provider.kind.needs_key() {
        return Err(missing_key(format!(
            "provider {name} has no API key: add it in Settings, Vault"
        )));
    }
    let rejected = if auth == UpstreamAuth::GcloudToken {
        format!(
            "Google rejected the access token for provider {name}: run `gcloud auth login` on \
             the daemon host, then retry"
        )
    } else {
        format!("provider {name} rejected its API key: replace it in Settings, Vault")
    };
    Ok(Route {
        api,
        upstream,
        auth,
        secret,
        chatgpt_account: None,
        openrouter: provider.kind == ProviderKind::Openrouter,
        rejected,
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
        unconfigured(
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
    let provider = match agent.provider_id {
        Some(id) => Some(state.store.get_provider(id).await),
        None => None,
    };
    let mut call = Call::new(
        &state,
        &agent,
        provider.as_ref().and_then(|p| p.as_ref().ok()),
        &path,
    );
    let refuse = |call: Call, status: StatusCode, message: &str| {
        call.record(UsageStatus::Refused, Tokens::default(), None);
        error(status, message)
    };
    let provider = match provider.transpose() {
        Ok(provider) => provider,
        Err(e) => {
            let message = format!("provider: {e}");
            return refuse(call, StatusCode::BAD_REQUEST, &message);
        }
    };
    if !clean(&path) {
        tracing::warn!(agent = %agent.name, "model call with an invalid path refused");
        return refuse(call, StatusCode::NOT_FOUND, "invalid path");
    }
    let route = match route(&state, provider, &path).await {
        Ok(route) => route,
        Err(failure) => {
            if failure.missing_key {
                let reason = failure.message.clone();
                credentials::set(&state, agent.id, CredentialProblem::Missing, reason).await;
            }
            return refuse(call, failure.status, &failure.message);
        }
    };
    if !permitted(&route.api, request.method(), &path) {
        // Visible on the host: an SDK update may call an endpoint not listed yet.
        tracing::warn!(agent = %agent.name, method = %request.method(), %path, "model call not proxied");
        return refuse(call, StatusCode::NOT_FOUND, "this API call is not proxied");
    }

    let (parts, body) = request.into_parts();
    let Ok(mut body) = axum::body::to_bytes(body, MAX_REQUEST_BYTES).await else {
        return refuse(
            call,
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large",
        );
    };
    let mut url = format!("{}/{path}", route.upstream.trim_end_matches('/'));
    if !below(&route.upstream, &url) {
        return refuse(call, StatusCode::NOT_FOUND, "invalid path");
    }
    let family = generation(&route.api, &path);
    if family.is_some() && body_names_model(&route.api) {
        if let Some(model) = state.call_model(agent.id).await {
            body = with_model(body, &model);
        }
        call.model_from_body(&body);
    }
    // Only the chat completions driver can drop every tool; the other harnesses keep their own.
    let sends_tools = family == Some(Family::ChatCompletions) && no_tools::sends_tools(&body);
    if let Some(q) = parts.uri.query() {
        url.push('?');
        url.push_str(q);
    }
    let upstream = upstream_request(&state.http, &parts, &url);
    let (upstream, body) = openrouter_extras(upstream, &route, agent.id, body);
    let upstream = with_credential(upstream, &route).body(body);

    tracing::debug!(agent = %agent.name, method = %parts.method, %path, "model request");
    let mut response = match upstream.send().await {
        Ok(r) => warned(agent.name.as_str(), &path, r),
        Err(e) => {
            tracing::warn!(agent = %agent.name, error = %e, "model upstream unreachable");
            if family.is_some() {
                call.record(UsageStatus::Error, Tokens::default(), None);
            }
            return error(StatusCode::BAD_GATEWAY, "model upstream unreachable");
        }
    };
    if route.secret.is_some() && refuses_credential(&route.api, response.status()) {
        let reason = route.rejected;
        credentials::set(&state, agent.id, CredentialProblem::Rejected, reason).await;
    }
    if sends_tools {
        response = no_tools::check(&state, &agent, &call.model, response).await;
    }
    respond(response, family.map(|f| (f, call)))
}

/// The agent gets the error body and shows why; Diagnostics gets the status.
/// The request to `url` with the SDK headers it needs from the agent's, and no others.
fn upstream_request(
    http: &reqwest::Client,
    parts: &axum::http::request::Parts,
    url: &str,
) -> reqwest::RequestBuilder {
    let mut upstream = http.request(parts.method.clone(), url);
    for (name, value) in &parts.headers {
        if listed(name.as_str(), FORWARD_REQUEST, FORWARD_REQUEST_PREFIXES) {
            upstream = upstream.header(name, value);
        }
    }
    upstream
}

fn warned(agent: &str, path: &str, response: reqwest::Response) -> reqwest::Response {
    let status = response.status();
    if !status.is_success() {
        tracing::warn!(agent, %status, path, "model upstream answered an error");
    }
    response
}

/// Adds the route's real credential, which never leaves the daemon.
fn with_credential(
    mut upstream: reqwest::RequestBuilder,
    route: &Route,
) -> reqwest::RequestBuilder {
    if let Some(secret) = &route.secret {
        upstream = match route.auth {
            UpstreamAuth::ApiKeyHeader => upstream.header("x-api-key", secret),
            UpstreamAuth::GoogApiKey => upstream.header("x-goog-api-key", secret),
            UpstreamAuth::Bearer | UpstreamAuth::GcloudToken => {
                upstream.header(header::AUTHORIZATION, format!("Bearer {secret}"))
            }
        };
    }
    if let Some(account) = &route.chatgpt_account {
        upstream = upstream.header("chatgpt-account-id", account);
    }
    upstream
}

/// Whether the upstream refused the credential. A 403 means that only on Bedrock (AWS answers
/// bad keys with it); elsewhere it is moderation, a missing permission or a disabled API.
fn refuses_credential(api: &Api, status: StatusCode) -> bool {
    status == StatusCode::UNAUTHORIZED
        || (status == StatusCode::FORBIDDEN && matches!(api, Api::Bedrock | Api::BedrockControl))
}

/// The upstream's response for the agent; a model run is metered as it streams.
fn respond(response: reqwest::Response, metered: Option<(Family, Call)>) -> Response {
    let status = response.status();
    let mut out = Response::builder().status(status.as_u16());
    for (name, value) in response.headers() {
        if listed(name.as_str(), FORWARD_RESPONSE, FORWARD_RESPONSE_PREFIXES) {
            out = out.header(name.clone(), value.clone());
        }
    }
    let stream = response.bytes_stream();
    let body = match metered {
        Some((family, call)) => {
            let content_type = out
                .headers_ref()
                .and_then(|h| h.get(header::CONTENT_TYPE))
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            let mut meter = Meter::new(call, family, status.is_success(), content_type);
            let mut stream = Box::pin(stream);
            Body::from_stream(futures_util::stream::poll_fn(move |cx| {
                let next = stream.poll_next_unpin(cx);
                if let Poll::Ready(chunk) = &next {
                    meter.feed(chunk.as_ref());
                }
                next
            }))
        }
        None => Body::from_stream(stream),
    };
    out.body(body)
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

    #[test]
    fn only_model_runs_are_metered() {
        let vertex = Api::Vertex {
            project: "p".into(),
            region: "r".into(),
        };
        let base = "projects/p/locations/r/publishers/anthropic/models";
        let (stream, count) = (
            format!("{base}/m:streamRawPredict"),
            format!("{base}/count-tokens:rawPredict"),
        );
        for (api, path, family) in [
            (&Api::Anthropic, "v1/messages", Some(Family::Anthropic)),
            (&Api::Anthropic, "v1/messages/count_tokens", None),
            (&Api::Anthropic, "v1/models", None),
            (
                &Api::Bedrock,
                "model/x/invoke-with-response-stream",
                Some(Family::Anthropic),
            ),
            (&Api::Bedrock, "model/x/count-tokens", None),
            (&vertex, stream.as_str(), Some(Family::Anthropic)),
            (&vertex, count.as_str(), None),
            (&Api::ChatCompletions, "models", None),
            (&Api::Responses, "responses", Some(Family::Responses)),
            (&Api::GeminiNative, "v1beta/models/g:countTokens", None),
            (
                &Api::GeminiNative,
                "v1beta/models/g:generateContent",
                Some(Family::Gemini),
            ),
        ] {
            assert_eq!(generation(api, path), family, "{path}");
        }
        assert!(body_names_model(&Api::Anthropic) && body_names_model(&Api::ChatCompletions));
        for api in [&Api::Bedrock, &vertex, &Api::GeminiNative] {
            assert!(!body_names_model(api), "the path names the model");
        }
    }
}
