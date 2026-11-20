//! Usage capture for the model proxy: token counts read from the upstream's
//! response while it streams past. Only counts and metadata are kept, never
//! prompts or completions.

use std::time::Instant;

use axum::body::Bytes;
use ferrobot_core::{Agent, AgentId, Provider};
use ferrobot_store::{Store, Tokens, UsageRecord, UsageStatus};
use serde_json::Value;

use crate::AppState;

/// Largest SSE line or JSON body kept for reading usage; bytes pass on regardless.
const MAX_KEPT: usize = 2 * 1024 * 1024;
/// Longest model id taken from a request.
const MAX_MODEL: usize = 200;

/// Where an API reports usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    /// Anthropic Messages, also on Bedrock and Vertex.
    Anthropic,
    ChatCompletions,
    /// `OpenAI` Responses (Codex).
    Responses,
    /// Gemini `generateContent`.
    Gemini,
}

/// One proxied model call by a known agent; becomes one usage row.
#[derive(Debug)]
pub(crate) struct Call {
    store: Store,
    started: Instant,
    agent_id: AgentId,
    agent_name: String,
    provider_id: Option<uuid::Uuid>,
    provider_kind: String,
    pub model: String,
}

/// The model named in a request path (Bedrock `model/<id>/`, Vertex and Gemini `models/<m>:`).
fn model_in_path(path: &str) -> Option<&str> {
    path.strip_prefix("model/")
        .and_then(|r| r.split('/').next())
        .or_else(|| {
            path.rsplit_once("models/")
                .and_then(|(_, r)| r.split(':').next())
        })
        .filter(|m| !m.is_empty() && m.len() <= MAX_MODEL)
}

impl Call {
    /// `provider` is `None` when the agent has none or it could not be loaded.
    pub fn new(state: &AppState, agent: &Agent, provider: Option<&Provider>, path: &str) -> Self {
        let provider_kind = match (agent.provider_id, provider) {
            (None, _) => "anthropic",
            (Some(_), Some(p)) => p.kind.as_str(),
            (Some(_), None) => "unknown",
        };
        let model = model_in_path(path)
            .map(str::to_owned)
            .or_else(|| agent.model.clone())
            .or_else(|| provider.and_then(|p| p.default_model.clone()))
            .or_else(|| state.settings.model.clone())
            .unwrap_or_else(|| "unknown".into());
        Self {
            store: state.store.clone(),
            started: Instant::now(),
            agent_id: agent.id,
            agent_name: agent.name.to_string(),
            provider_id: agent.provider_id,
            provider_kind: provider_kind.into(),
            model,
        }
    }

    /// Takes the request body's `model`, which beats the path and the agent's setting.
    pub fn model_from_body(&mut self, body: &[u8]) {
        #[derive(serde::Deserialize)]
        struct Named {
            model: Option<String>,
        }
        if let Ok(Named { model: Some(m) }) = serde_json::from_slice(body)
            && !m.is_empty()
            && m.len() <= MAX_MODEL
        {
            self.model = m;
        }
    }

    /// Writes the row off the request path.
    pub fn record(self, status: UsageStatus, tokens: Tokens) {
        let row = UsageRecord {
            at: time::OffsetDateTime::now_utc(),
            agent_id: self.agent_id,
            agent_name: self.agent_name,
            provider_id: self.provider_id,
            provider_kind: self.provider_kind,
            model: self.model,
            tokens,
            latency_ms: i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX),
            status,
        };
        // A drop during shutdown has no runtime left; that row is lost.
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let store = self.store;
            rt.spawn(async move {
                if let Err(e) = store.insert_usage(row).await {
                    tracing::warn!(error = %e, "cannot record model usage");
                }
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Sse,
    Json,
    Ignore,
}

/// Watches a response body and records the call when the body is dropped:
/// at its end, or early when the agent goes away.
#[derive(Debug)]
pub(crate) struct Meter {
    call: Option<Call>,
    family: Family,
    mode: Mode,
    status: UsageStatus,
    kept: Vec<u8>,
    /// The current SSE line, or the JSON body, outgrew [`MAX_KEPT`].
    overflow: bool,
    /// Usage may have been in an oversized line: counts are not trusted.
    lost: bool,
    tokens: Tokens,
}

impl Meter {
    pub fn new(call: Call, family: Family, http_ok: bool, content_type: &str) -> Self {
        let mode = if !http_ok {
            Mode::Ignore
        } else if content_type.starts_with("text/event-stream") {
            Mode::Sse
        } else if content_type.starts_with("application/vnd.amazon.eventstream") {
            // shortcut: Bedrock's binary event stream is not decoded, so these calls are not tracked; decode its frames if Bedrock is used much.
            Mode::Ignore
        } else {
            Mode::Json
        };
        Self {
            call: Some(call),
            family,
            mode,
            status: if http_ok {
                UsageStatus::Ok
            } else {
                UsageStatus::Error
            },
            kept: Vec::new(),
            overflow: false,
            lost: false,
            tokens: Tokens::default(),
        }
    }

    pub fn feed(&mut self, chunk: &Result<Bytes, reqwest::Error>) {
        let Ok(mut bytes) = chunk.as_deref() else {
            self.status = UsageStatus::Error;
            return;
        };
        match self.mode {
            Mode::Ignore => {}
            Mode::Json => self.keep(bytes),
            Mode::Sse => {
                while let Some(i) = bytes.iter().position(|b| *b == b'\n') {
                    self.keep(&bytes[..i]);
                    self.end_line();
                    bytes = &bytes[i + 1..];
                }
                self.keep(bytes);
            }
        }
    }

    fn keep(&mut self, part: &[u8]) {
        if self.kept.len() + part.len() > MAX_KEPT {
            self.overflow = true;
            self.kept = Vec::new();
        } else if !self.overflow {
            self.kept.extend_from_slice(part);
        }
    }

    fn end_line(&mut self) {
        let line = std::mem::take(&mut self.kept);
        if std::mem::take(&mut self.overflow) {
            // shortcut: an SSE line over 2 MiB (e.g. a huge Codex `response.completed`) drops the call's counts; stream-parse JSON if that happens.
            self.lost = true;
        } else if let Some(data) = line.strip_prefix(b"data:")
            && data.windows(6).any(|w| w == b"\"usage")
            && let Ok(v) = serde_json::from_slice::<Value>(data)
        {
            read(self.family, &v, &mut self.tokens);
        }
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        match self.mode {
            Mode::Sse if !self.kept.is_empty() || self.overflow => self.end_line(),
            Mode::Json if self.overflow => self.lost = true,
            Mode::Json => {
                if let Ok(v) = serde_json::from_slice::<Value>(&self.kept) {
                    // Gemini's streamGenerateContent without `alt=sse` is one JSON array.
                    for item in v.as_array().map_or(std::slice::from_ref(&v), Vec::as_slice) {
                        read(self.family, item, &mut self.tokens);
                    }
                }
            }
            _ => {}
        }
        let tokens = if self.lost {
            Tokens::default()
        } else {
            self.tokens
        };
        if let Some(call) = self.call.take() {
            call.record(self.status, tokens);
        }
    }
}

fn count(v: &Value, pointer: &str) -> Option<i64> {
    v.pointer(pointer).and_then(Value::as_i64)
}

/// Overwrites `slot` only when the field is reported (streams repeat cumulative counts).
fn set(slot: &mut Option<i64>, v: &Value, pointer: &str) {
    if let Some(n) = count(v, pointer) {
        *slot = Some(n);
    }
}

/// `OpenAI`-style counts include cached tokens in the input; split them out.
fn split_cached(t: &mut Tokens, input: Option<i64>, output: Option<i64>, cached: Option<i64>) {
    if let Some(input) = input {
        t.input = Some(input.saturating_sub(cached.unwrap_or(0)));
        t.cache_read = cached;
    }
    if output.is_some() {
        t.output = output;
    }
}

/// Reads the usage fields of one response object or stream event into `t`.
fn read(family: Family, v: &Value, t: &mut Tokens) {
    let object = |p: &str| v.pointer(p).filter(|u| u.is_object());
    match family {
        Family::Anthropic => {
            // `message_start` nests it under `message`; `message_delta` and bodies do not.
            for u in [object("/usage"), object("/message/usage")]
                .into_iter()
                .flatten()
            {
                set(&mut t.input, u, "/input_tokens");
                set(&mut t.output, u, "/output_tokens");
                set(&mut t.cache_read, u, "/cache_read_input_tokens");
                set(&mut t.cache_write, u, "/cache_creation_input_tokens");
            }
        }
        Family::ChatCompletions => {
            if let Some(u) = object("/usage") {
                let cached = count(u, "/prompt_tokens_details/cached_tokens");
                split_cached(
                    t,
                    count(u, "/prompt_tokens"),
                    count(u, "/completion_tokens"),
                    cached,
                );
            }
        }
        Family::Responses => {
            if let Some(u) = object("/response/usage").or_else(|| object("/usage")) {
                let cached = count(u, "/input_tokens_details/cached_tokens");
                split_cached(
                    t,
                    count(u, "/input_tokens"),
                    count(u, "/output_tokens"),
                    cached,
                );
            }
        }
        Family::Gemini => {
            if let Some(u) = object("/usageMetadata") {
                let (candidates, thoughts) = (
                    count(u, "/candidatesTokenCount"),
                    count(u, "/thoughtsTokenCount"),
                );
                let output = (candidates.is_some() || thoughts.is_some())
                    .then(|| candidates.unwrap_or(0) + thoughts.unwrap_or(0));
                let cached = count(u, "/cachedContentTokenCount");
                split_cached(t, count(u, "/promptTokenCount"), output, cached);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tokens(family: Family, events: &[Value]) -> Tokens {
        let mut t = Tokens::default();
        for e in events {
            read(family, e, &mut t);
        }
        t
    }

    fn of(input: i64, output: i64, cache_read: Option<i64>, cache_write: Option<i64>) -> Tokens {
        Tokens {
            input: Some(input),
            output: Some(output),
            cache_read,
            cache_write,
        }
    }

    #[test]
    fn usage_is_read_per_api_family() {
        let anthropic = tokens(
            Family::Anthropic,
            &[
                json!({"type": "message_start", "message": {"usage": {"input_tokens": 10,
                    "output_tokens": 1, "cache_read_input_tokens": 300,
                    "cache_creation_input_tokens": 40}}}),
                json!({"type": "message_delta", "usage": {"output_tokens": 25}}),
            ],
        );
        assert_eq!(anthropic, of(10, 25, Some(300), Some(40)));

        let chat = tokens(
            Family::ChatCompletions,
            &[
                json!({"choices": [{"delta": {"content": "hi"}}], "usage": null}),
                json!({"choices": [], "usage": {"prompt_tokens": 100, "completion_tokens": 7,
                    "prompt_tokens_details": {"cached_tokens": 60}}}),
            ],
        );
        assert_eq!(chat, of(40, 7, Some(60), None));
        let ollama = tokens(
            Family::ChatCompletions,
            &[json!({"usage": {"prompt_tokens": 9, "completion_tokens": 3}})],
        );
        assert_eq!(ollama, of(9, 3, None, None));

        let codex = tokens(
            Family::Responses,
            &[
                json!({"type": "response.created", "response": {"usage": null}}),
                json!({"type": "response.completed", "response": {"usage": {"input_tokens": 50,
                    "output_tokens": 8, "input_tokens_details": {"cached_tokens": 20}}}}),
            ],
        );
        assert_eq!(codex, of(30, 8, Some(20), None));

        let gemini = tokens(
            Family::Gemini,
            &[
                json!({"usageMetadata": {"promptTokenCount": 70, "candidatesTokenCount": 2}}),
                json!({"usageMetadata": {"promptTokenCount": 70, "candidatesTokenCount": 9,
                    "thoughtsTokenCount": 4, "cachedContentTokenCount": 50}}),
            ],
        );
        assert_eq!(gemini, of(20, 13, Some(50), None));
    }

    #[test]
    fn models_come_from_the_path() {
        assert_eq!(
            model_in_path("model/eu.anthropic.claude-x-v1:0/invoke"),
            Some("eu.anthropic.claude-x-v1:0")
        );
        assert_eq!(
            model_in_path("projects/p/locations/r/publishers/anthropic/models/m@2025:rawPredict"),
            Some("m@2025")
        );
        assert_eq!(
            model_in_path("v1beta/models/gemini-x:streamGenerateContent"),
            Some("gemini-x")
        );
        assert_eq!(model_in_path("chat/completions"), None);
    }
}
