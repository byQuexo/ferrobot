//! Usage capture for the model proxy: token counts read from the response as it streams past.
//! Only counts and metadata are kept, never prompts or completions.

use std::time::Instant;

use axum::body::Bytes;
use ferrobot_core::{Agent, AgentId, Provider};
use ferrobot_store::{Store, Tokens, UsageRecord, UsageStatus};
use serde_json::Value;

use crate::AppState;

/// Largest SSE line or JSON body kept for reading usage; bytes pass on regardless.
const MAX_KEPT: usize = 2 * 1024 * 1024;
/// Longest model id stored.
const MAX_MODEL: usize = 200;
/// A reported cost above this (USD per call) is not believed.
const MAX_REPORTED_COST: f64 = 1000.0;

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
    /// The agent's turn when the call began, for its trace.
    turn_id: Option<String>,
    /// The request's parts by size, for the context breakdown.
    sizes: Option<crate::context::Sizes>,
}

/// The model named in a request path (Bedrock `model/<id>/`, Vertex and Gemini `models/<m>:`).
fn model_in_path(path: &str) -> Option<&str> {
    path.strip_prefix("model/")
        .and_then(|r| r.split('/').next())
        .or_else(|| {
            path.rsplit_once("models/")
                .and_then(|(_, r)| r.split(':').next())
        })
        .filter(|m| !m.is_empty())
}

/// The model id as stored: agent-written text, so no control characters and at most [`MAX_MODEL`] bytes.
fn stored_model(model: &str, status: UsageStatus) -> String {
    let mut m = String::new();
    for c in model.chars().filter(|c| !c.is_control()) {
        if m.len() + c.len_utf8() > MAX_MODEL {
            break;
        }
        m.push(c);
    }
    if status == UsageStatus::Refused || m.is_empty() {
        "unknown".into()
    } else {
        m
    }
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
            turn_id: state.hub.current_turn(agent.id),
            sizes: None,
        }
    }

    /// Measures a model run's request parts, so a successful call can show what fills the
    /// context; other calls (`family` is `None`) have none.
    pub fn measure(&mut self, family: Option<Family>, body: &[u8]) {
        self.sizes = family.and_then(|f| crate::context::measure(f, body));
    }

    /// Takes the request body's `model`, which beats the path and the agent's setting.
    pub fn model_from_body(&mut self, body: &[u8]) {
        #[derive(serde::Deserialize)]
        struct Named {
            model: Option<String>,
        }
        if let Ok(Named { model: Some(m) }) = serde_json::from_slice(body)
            && !m.is_empty()
        {
            self.model = m;
        }
    }

    /// Writes the row off the request path.
    pub fn record(self, status: UsageStatus, tokens: Tokens, cost_usd: Option<f64>) {
        if let (UsageStatus::Ok, Some(input), Some(sizes)) = (status, tokens.input, self.sizes) {
            let context = input + tokens.cache_read.unwrap_or(0) + tokens.cache_write.unwrap_or(0);
            crate::context::remember(
                self.agent_id,
                &stored_model(&self.model, status),
                context,
                sizes,
            );
        }
        let row = UsageRecord {
            at: time::OffsetDateTime::now_utc(),
            agent_id: self.agent_id,
            agent_name: self.agent_name,
            provider_id: self.provider_id,
            provider_kind: self.provider_kind,
            model: stored_model(&self.model, status),
            tokens,
            cost_usd,
            latency_ms: i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX),
            status,
            turn_id: self.turn_id,
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
/// at its end, or early when the agent goes away. `Debug` never shows response bytes.
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
    /// What `OpenRouter` said the call cost; only its responses are believed.
    cost: Option<f64>,
    /// The upstream body reached its end.
    ended: bool,
}

impl std::fmt::Debug for Meter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Meter")
            .field("status", &self.status)
            .field("tokens", &self.tokens)
            .finish_non_exhaustive()
    }
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
            cost: None,
            ended: false,
        }
    }

    fn read(&mut self, v: &Value) {
        read(self.family, v, &mut self.tokens);
        let openrouter = self
            .call
            .as_ref()
            .is_some_and(|c| c.provider_kind == "openrouter");
        if openrouter && let Some(cost) = reported_cost(v) {
            self.cost = Some(cost);
        }
    }

    /// One poll result of the upstream body; `None` is its end.
    pub fn feed(&mut self, chunk: Option<&Result<Bytes, reqwest::Error>>) {
        let Some(chunk) = chunk else {
            self.ended = true;
            return;
        };
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
        let has = |data: &[u8], needle: &[u8]| data.windows(needle.len()).any(|w| w == needle);
        if std::mem::take(&mut self.overflow) {
            // shortcut: an SSE line over 2 MiB (e.g. a huge Codex `response.completed`) drops the call's counts; stream-parse JSON if that happens.
            self.lost = true;
        } else if line
            .strip_prefix(b"event:")
            .is_some_and(|e| e.trim_ascii() == b"error")
        {
            self.status = UsageStatus::Error;
        } else if let Some(data) = line.strip_prefix(b"data:") {
            let json = || serde_json::from_slice::<Value>(data).ok();
            // Drivers stop reading at the protocol's last event, often before the upstream closes.
            if data.trim_ascii() == b"[DONE]"
                || ((has(data, b"message_stop") || has(data, b"response.completed"))
                    && json().is_some_and(|v| {
                        v["type"] == "message_stop" || v["type"] == "response.completed"
                    }))
            {
                self.ended = true;
            }
            if has(data, b"\"error\"")
                && json().is_some_and(|v| v["type"] == "error" || v["error"].is_object())
            {
                self.status = UsageStatus::Error;
            } else if has(data, b"\"usage")
                && let Some(v) = json()
            {
                self.read(&v);
            }
        }
    }

    /// Status, counts and reported cost once the body is gone; a body that never reached its
    /// end is an error.
    fn finish(&mut self) -> (UsageStatus, Tokens, Option<f64>) {
        match self.mode {
            Mode::Sse if !self.kept.is_empty() || self.overflow => self.end_line(),
            Mode::Json if self.overflow => self.lost = true,
            Mode::Json => {
                if let Ok(v) = serde_json::from_slice::<Value>(&self.kept) {
                    // Gemini's streamGenerateContent without `alt=sse` is one JSON array.
                    for item in v.as_array().map_or(std::slice::from_ref(&v), Vec::as_slice) {
                        self.read(item);
                    }
                }
            }
            _ => {}
        }
        if !self.ended {
            self.status = UsageStatus::Error;
        }
        if self.lost {
            (self.status, Tokens::default(), None)
        } else {
            (self.status, self.tokens, self.cost)
        }
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        if let Some(call) = self.call.take() {
            let (status, tokens, cost) = self.finish();
            call.record(status, tokens, cost);
        }
    }
}

/// `OpenRouter`'s `usage.cost`: what it charged for the call, in USD.
fn reported_cost(v: &Value) -> Option<f64> {
    v.pointer("/usage/cost")
        .and_then(Value::as_f64)
        .filter(|c| (0.0..=MAX_REPORTED_COST).contains(c))
}

/// A reported count, clamped so no sum or cost can overflow or go negative.
fn count(v: &Value, pointer: &str) -> Option<i64> {
    v.pointer(pointer)
        .and_then(Value::as_i64)
        .map(|n| n.clamp(0, i64::from(u32::MAX)))
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
        t.input = Some(input.saturating_sub(cached.unwrap_or(0)).max(0));
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
                let prompt = count(u, "/prompt_tokens");
                let mut output = count(u, "/completion_tokens");
                // Gemini's OpenAI endpoint leaves thinking tokens out of completion_tokens, not out of the total.
                if let (Some(total), Some(prompt)) = (count(u, "/total_tokens"), prompt) {
                    output = Some(output.unwrap_or(0).max(total - prompt));
                }
                let cached = count(u, "/prompt_tokens_details/cached_tokens");
                split_cached(t, prompt, output, cached);
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
    fn only_a_plausible_reported_cost_counts() {
        let cost = |c: Value| reported_cost(&json!({"usage": {"prompt_tokens": 1, "cost": c}}));
        assert_eq!(cost(json!(0.0042)), Some(0.0042));
        assert_eq!(cost(json!(0)), Some(0.0));
        for bad in [json!(-1), json!(1e9), json!("0.1"), Value::Null] {
            assert_eq!(cost(bad.clone()), None, "{bad}");
        }
        assert_eq!(reported_cost(&json!({"usage": {"prompt_tokens": 1}})), None);
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

        let gemini_openai = tokens(
            Family::ChatCompletions,
            &[json!({"usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 40}})],
        );
        assert_eq!(
            gemini_openai,
            of(10, 30, None, None),
            "thinking tokens count"
        );
    }

    #[test]
    fn counts_are_clamped() {
        let huge = tokens(
            Family::ChatCompletions,
            &[
                json!({"usage": {"prompt_tokens": i64::MAX, "completion_tokens": -5,
                "total_tokens": -1, "prompt_tokens_details": {"cached_tokens": i64::MAX}}}),
            ],
        );
        assert_eq!(huge, of(0, 0, Some(u32::MAX.into()), None));
        let negative = tokens(
            Family::Anthropic,
            &[json!({"usage": {"input_tokens": -3, "output_tokens": i64::MAX}})],
        );
        assert_eq!(negative.input, Some(0));
        assert_eq!(negative.output, Some(u32::MAX.into()));
        let cached_over_prompt = tokens(
            Family::Responses,
            &[json!({"usage": {"input_tokens": 5, "output_tokens": 1,
                "input_tokens_details": {"cached_tokens": 9}}})],
        );
        assert_eq!(cached_over_prompt, of(0, 1, Some(9), None));
    }

    #[test]
    fn stored_models_are_plain_and_short() {
        assert_eq!(stored_model("gpt-5", UsageStatus::Ok), "gpt-5");
        assert_eq!(stored_model("gpt-5", UsageStatus::Refused), "unknown");
        assert_eq!(stored_model("a\nb\u{7}c", UsageStatus::Error), "abc");
        assert_eq!(stored_model("\r\n", UsageStatus::Ok), "unknown");
        let long = format!("{}é", "x".repeat(199));
        assert_eq!(stored_model(&long, UsageStatus::Ok), "x".repeat(199));
    }

    /// A meter for a 200 response that records nothing when dropped.
    fn meter(family: Family, content_type: &str) -> Meter {
        let call = Call {
            store: Store::open_in_memory().unwrap(),
            started: Instant::now(),
            agent_id: AgentId::new(),
            agent_name: "ann".into(),
            provider_id: None,
            provider_kind: "anthropic".into(),
            model: "m".into(),
            turn_id: None,
            sizes: None,
        };
        let mut m = Meter::new(call, family, true, content_type);
        m.call = None;
        m
    }

    #[test]
    fn only_a_stream_that_ends_cleanly_is_ok() {
        let usage = "event: message_start\ndata: {\"type\":\"message_start\",\
            \"message\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}\n\n";
        let error = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}\n\n";
        let run = |chunks: &[&'static str], end: bool| {
            let mut m = meter(Family::Anthropic, "text/event-stream");
            for &c in chunks {
                m.feed(Some(&Ok(Bytes::from_static(c.as_bytes()))));
            }
            if end {
                m.feed(None);
            }
            m.finish()
        };
        let counted = Tokens {
            input: Some(12),
            output: Some(1),
            ..Tokens::default()
        };
        assert_eq!(run(&[usage], true), (UsageStatus::Ok, counted, None));
        assert_eq!(
            run(&[usage], false),
            (UsageStatus::Error, counted, None),
            "dropped before the end"
        );
        assert_eq!(run(&[usage, error], true).0, UsageStatus::Error);
        let chat_error = "data: {\"error\":{\"message\":\"upstream failed\"}}\n\n";
        assert_eq!(run(&[chat_error], true).0, UsageStatus::Error);
        let mentions_error =
            "data: {\"choices\":[{\"delta\":{\"content\":\"\\\"error\\\"\"}}]}\n\n";
        assert_eq!(run(&[mentions_error], true).0, UsageStatus::Ok);
        let stop = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        assert_eq!(
            run(&[usage, stop], false),
            (UsageStatus::Ok, counted, None),
            "the agent may hang up after the last event"
        );
        let mut chat = meter(Family::ChatCompletions, "text/event-stream");
        chat.feed(Some(&Ok(Bytes::from_static(b"data: [DONE]\n\n"))));
        assert_eq!(chat.finish().0, UsageStatus::Ok);
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
