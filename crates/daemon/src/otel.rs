//! Sends each finished turn's trace to the operator's OTLP/HTTP endpoint (Phoenix, Langfuse or
//! any OpenTelemetry backend), encoded as protobuf by hand: Phoenix accepts nothing else, and
//! the few messages needed do not warrant the OpenTelemetry SDK.

use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::Duration;

use ferrobot_core::{AgentId, ProviderKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::AppState;
use crate::error::ApiError;
use crate::trace::{Span, SpanKind, Trace};

const SETTING: &str = "otel";
/// The secret store entry holding the auth header's value.
pub const SECRET: &str = "otel:header";
/// Usage rows of a Claude Code turn land as its stream ends, just before the turn does.
const SETTLE: Duration = Duration::from_secs(2);
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT_CHARS: usize = 8000;

/// Where finished turns' traces go. The auth header's value is in the secret store.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct OtelSettings {
    pub enabled: bool,
    /// The OTLP/HTTP traces URL, e.g. `http://localhost:6006/v1/traces`.
    pub endpoint: String,
    /// The auth header's name, e.g. `Authorization`; `null`: none.
    pub header: Option<String>,
    /// Sent as the `openinference.project.name` resource attribute (Phoenix's project).
    pub project: Option<String>,
    /// Also send tool inputs and the turn's reply; off: only names, times, tokens and errors.
    pub content: bool,
}

/// How the last export went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ExportStatus {
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub at: OffsetDateTime,
    /// Why it failed; `null` when it went through.
    pub error: Option<String>,
}

static LAST: LazyLock<Mutex<Option<ExportStatus>>> = LazyLock::new(Mutex::default);

pub fn last_export() -> Option<ExportStatus> {
    LAST.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

pub async fn load(state: &AppState) -> Result<OtelSettings, ApiError> {
    match state.store.get_setting(SETTING).await? {
        Some(raw) => serde_json::from_str(&raw)
            .map_err(|e| ApiError::Internal(format!("trace export settings: {e}"))),
        None => Ok(OtelSettings::default()),
    }
}

pub async fn save(state: &AppState, settings: &OtelSettings) -> Result<(), ApiError> {
    let raw = serde_json::to_string(settings).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(state.store.put_setting(SETTING, raw).await?)
}

/// Trims and checks the settings.
pub fn validate(s: &mut OtelSettings) -> Result<(), String> {
    s.endpoint = s.endpoint.trim().to_owned();
    let trimmed = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    s.header = trimmed(&s.header);
    s.project = trimmed(&s.project);
    if !s.endpoint.is_empty() {
        let url = reqwest::Url::parse(&s.endpoint).map_err(|_| "the endpoint is not a URL")?;
        if !matches!(url.scheme(), "http" | "https") || s.endpoint.len() > 2048 {
            return Err("the endpoint must be an http or https URL".into());
        }
    } else if s.enabled {
        return Err("export needs an endpoint".into());
    }
    if s.header
        .as_deref()
        .is_some_and(|h| h.len() > 64 || !h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    {
        return Err("the header name has letters, digits and dashes only".into());
    }
    if s.project
        .as_deref()
        .is_some_and(|p| p.chars().count() > 100 || p.chars().any(char::is_control))
    {
        return Err("the project name has at most 100 characters".into());
    }
    Ok(())
}

/// Exports `turn` in the background once it has ended, when export is on.
pub fn turn_done(state: &AppState, agent: AgentId, turn: String) {
    let state = state.clone();
    tokio::spawn(async move {
        let Ok(settings) = load(&state).await else {
            return;
        };
        if !settings.enabled || settings.endpoint.is_empty() {
            return;
        }
        tokio::time::sleep(SETTLE).await;
        let error = export(&state, &settings, agent, &turn).await.err();
        if let Some(e) = &error {
            tracing::warn!(%turn, error = %e, "trace export failed");
        }
        *LAST.lock().unwrap_or_else(PoisonError::into_inner) = Some(ExportStatus {
            at: OffsetDateTime::now_utc(),
            error,
        });
    });
}

async fn export(
    state: &AppState,
    settings: &OtelSettings,
    agent: AgentId,
    turn: &str,
) -> Result<(), String> {
    let agent = state
        .store
        .get_agent(agent)
        .await
        .map_err(|e| e.to_string())?;
    let events = state
        .store
        .turn_events(agent.id, turn.to_owned())
        .await
        .map_err(|e| e.to_string())?;
    let calls = state
        .store
        .turn_calls(agent.id, turn.to_owned())
        .await
        .map_err(|e| e.to_string())?;
    let claude_code = match agent.provider_id {
        Some(p) => state
            .store
            .get_provider(p)
            .await
            .is_ok_and(|p| p.kind == ProviderKind::ClaudeCode),
        None => false,
    };
    let models_tracked =
        !claude_code || !calls.is_empty() || events.iter().any(|e| e.kind == "model_call");
    let Some(trace) = crate::trace::build(turn, &events, &calls, models_tracked) else {
        return Ok(());
    };
    let output = events
        .iter()
        .rev()
        .find(|e| e.kind == "turn_done")
        .and_then(|e| e.payload["result"].as_str())
        .map(|r| r.chars().take(MAX_OUTPUT_CHARS).collect::<String>());
    let body = encode(&TurnInfo {
        trace: &trace,
        agent_id: &agent.id.to_string(),
        agent_name: agent.name.as_str(),
        project: settings.project.as_deref(),
        output: output.as_deref(),
        content: settings.content,
    });
    let mut req = state
        .http
        .post(&settings.endpoint)
        .header("content-type", "application/x-protobuf")
        .timeout(TIMEOUT)
        .body(body);
    if let Some(name) = &settings.header
        && let Some(value) = state
            .settings
            .secrets
            .get(SECRET)
            .await
            .map_err(|e| e.to_string())?
    {
        req = req.header(name.as_str(), value);
    }
    let res = req.send().await.map_err(|e| e.without_url().to_string())?;
    if res.status().is_success() {
        Ok(())
    } else {
        Err(format!("the endpoint answered {}", res.status()))
    }
}

struct TurnInfo<'a> {
    trace: &'a Trace,
    agent_id: &'a str,
    agent_name: &'a str,
    project: Option<&'a str>,
    output: Option<&'a str>,
    content: bool,
}

enum Value {
    Str(String),
    Int(i64),
    Float(f64),
}

fn text(s: &str) -> Value {
    Value::Str(s.to_owned())
}

/// Protobuf wire format: just what `ExportTraceServiceRequest` needs.
mod pb {
    pub fn varint(buf: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            buf.push(u8::try_from(v & 0x7f).unwrap_or_default() | 0x80);
            v >>= 7;
        }
        buf.push(u8::try_from(v).unwrap_or_default());
    }

    fn key(buf: &mut Vec<u8>, field: u32, wire: u8) {
        varint(buf, u64::from(field) << 3 | u64::from(wire));
    }

    pub fn uint(buf: &mut Vec<u8>, field: u32, v: u64) {
        key(buf, field, 0);
        varint(buf, v);
    }

    pub fn fixed64(buf: &mut Vec<u8>, field: u32, v: u64) {
        key(buf, field, 1);
        buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn bytes(buf: &mut Vec<u8>, field: u32, v: &[u8]) {
        key(buf, field, 2);
        varint(buf, v.len() as u64);
        buf.extend_from_slice(v);
    }
}

fn attr(buf: &mut Vec<u8>, field: u32, key: &str, value: &Value) {
    let mut any = Vec::new();
    match value {
        Value::Str(s) => pb::bytes(&mut any, 1, s.as_bytes()),
        Value::Int(i) => pb::uint(&mut any, 3, i.cast_unsigned()),
        Value::Float(f) => pb::fixed64(&mut any, 4, f.to_bits()),
    }
    let mut kv = Vec::new();
    pb::bytes(&mut kv, 1, key.as_bytes());
    pb::bytes(&mut kv, 2, &any);
    pb::bytes(buf, field, &kv);
}

fn id(seed: &str, len: usize) -> Vec<u8> {
    Sha256::digest(seed.as_bytes())[..len].to_vec()
}

fn nanos(t: OffsetDateTime) -> u64 {
    u64::try_from(t.unix_timestamp_nanos()).unwrap_or_default()
}

struct SpanOut<'a> {
    id: Vec<u8>,
    parent: Option<&'a [u8]>,
    name: &'a str,
    /// 1 internal, 3 client.
    kind: u64,
    start: OffsetDateTime,
    end: OffsetDateTime,
    attrs: Vec<(&'static str, Value)>,
    error: Option<&'a str>,
}

fn span_bytes(trace_id: &[u8], s: &SpanOut<'_>) -> Vec<u8> {
    let mut buf = Vec::new();
    pb::bytes(&mut buf, 1, trace_id);
    pb::bytes(&mut buf, 2, &s.id);
    if let Some(parent) = s.parent {
        pb::bytes(&mut buf, 4, parent);
    }
    pb::bytes(&mut buf, 5, s.name.as_bytes());
    pb::uint(&mut buf, 6, s.kind);
    pb::fixed64(&mut buf, 7, nanos(s.start));
    pb::fixed64(&mut buf, 8, nanos(s.end.max(s.start)));
    for (k, v) in &s.attrs {
        attr(&mut buf, 9, k, v);
    }
    let mut status = Vec::new();
    match s.error {
        Some(message) => {
            pb::bytes(&mut status, 2, message.as_bytes());
            pb::uint(&mut status, 3, 2);
        }
        None => pb::uint(&mut status, 3, 1),
    }
    pb::bytes(&mut buf, 15, &status);
    buf
}

/// The step's span, with `OpenInference` attributes (Phoenix, Langfuse) and `gen_ai.*` ones
/// (Langfuse and other `OpenTelemetry` backends).
fn step<'a>(
    s: &'a Span,
    n: usize,
    turn: &str,
    root: &'a [u8],
    end: OffsetDateTime,
    content: bool,
) -> SpanOut<'a> {
    let mut attrs = Vec::new();
    let kind = match s.kind {
        SpanKind::Model => {
            let prompt = [s.input_tokens, s.cache_read_tokens, s.cache_write_tokens]
                .iter()
                .flatten()
                .sum::<i64>();
            attrs.extend([
                ("openinference.span.kind", text("LLM")),
                ("llm.model_name", text(&s.name)),
                ("gen_ai.operation.name", text("chat")),
                ("gen_ai.request.model", text(&s.name)),
                ("llm.token_count.prompt", Value::Int(prompt)),
                ("gen_ai.usage.input_tokens", Value::Int(prompt)),
            ]);
            if let Some(out) = s.output_tokens {
                attrs.push(("llm.token_count.completion", Value::Int(out)));
                attrs.push(("gen_ai.usage.output_tokens", Value::Int(out)));
            }
            if let Some(read) = s.cache_read_tokens {
                attrs.push((
                    "llm.token_count.prompt_details.cache_read",
                    Value::Int(read),
                ));
            }
            if let Some(write) = s.cache_write_tokens {
                attrs.push((
                    "llm.token_count.prompt_details.cache_write",
                    Value::Int(write),
                ));
            }
            if let Some(cost) = s.cost_usd {
                attrs.push(("ferrobot.cost_usd", Value::Float(cost)));
            }
            3
        }
        SpanKind::Tool => {
            attrs.extend([
                ("openinference.span.kind", text("TOOL")),
                ("tool.name", text(&s.name)),
                ("gen_ai.operation.name", text("execute_tool")),
                ("gen_ai.tool.name", text(&s.name)),
            ]);
            1
        }
    };
    if let Some(input) = s.input.as_ref().filter(|_| content) {
        attrs.push(("input.value", Value::Str(input.to_string())));
        attrs.push(("input.mime_type", text("application/json")));
    }
    SpanOut {
        id: id(&format!("{turn}/{n}"), 8),
        parent: Some(root),
        name: &s.name,
        kind,
        start: s.started_at,
        end: s.ended_at.unwrap_or(end),
        attrs,
        // A failed tool's error is its own output: content, so it only goes out with content on.
        error: s.error.as_deref().map(|e| match (content, s.kind) {
            (true, _) | (false, SpanKind::Model) => e,
            (false, SpanKind::Tool) if e.starts_with("denied") => "denied",
            (false, SpanKind::Tool) => "failed",
        }),
    }
}

/// An `ExportTraceServiceRequest` with the turn as the root span and its steps below it.
fn encode(t: &TurnInfo<'_>) -> Vec<u8> {
    let trace = t.trace;
    let end = trace.ended_at.unwrap_or(trace.started_at);
    let trace_id = id(&trace.turn_id, 16);
    let root_id = id(&format!("{}/turn", trace.turn_id), 8);
    let failed = trace.outcome.as_deref() == Some("failed");
    let mut root_attrs = vec![
        ("openinference.span.kind", text("AGENT")),
        ("gen_ai.operation.name", text("invoke_agent")),
        ("gen_ai.agent.name", text(t.agent_name)),
        ("langfuse.trace.name", text(t.agent_name)),
        ("session.id", text(t.agent_id)),
        ("ferrobot.agent.id", text(t.agent_id)),
        ("ferrobot.turn.id", text(&trace.turn_id)),
    ];
    if let Some(outcome) = &trace.outcome {
        root_attrs.push(("ferrobot.turn.outcome", text(outcome)));
    }
    if let Some(output) = t.output.filter(|_| t.content) {
        root_attrs.push(("output.value", text(output)));
    }
    let root = SpanOut {
        id: root_id.clone(),
        parent: None,
        name: t.agent_name,
        kind: 1,
        start: trace.started_at,
        end,
        attrs: root_attrs,
        error: failed.then(|| trace.error.as_deref().unwrap_or("the turn failed")),
    };
    let mut scope = Vec::new();
    let mut info = Vec::new();
    pb::bytes(&mut info, 1, b"ferrobot");
    pb::bytes(&mut info, 2, env!("CARGO_PKG_VERSION").as_bytes());
    pb::bytes(&mut scope, 1, &info);
    pb::bytes(&mut scope, 2, &span_bytes(&trace_id, &root));
    for (n, s) in trace.spans.iter().enumerate() {
        pb::bytes(
            &mut scope,
            2,
            &span_bytes(
                &trace_id,
                &step(s, n, &trace.turn_id, &root_id, end, t.content),
            ),
        );
    }
    let mut resource = Vec::new();
    attr(&mut resource, 1, "service.name", &text("ferrobot"));
    attr(
        &mut resource,
        1,
        "service.version",
        &text(env!("CARGO_PKG_VERSION")),
    );
    if let Some(project) = t.project {
        attr(
            &mut resource,
            1,
            "openinference.project.name",
            &text(project),
        );
    }
    let mut spans = Vec::new();
    pb::bytes(&mut spans, 1, &resource);
    pb::bytes(&mut spans, 2, &scope);
    let mut request = Vec::new();
    pb::bytes(&mut request, 1, &spans);
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits one message level into (field, wire, payload) for length-delimited fields.
    fn fields(mut buf: &[u8]) -> Vec<(u64, Vec<u8>)> {
        let mut out = Vec::new();
        let varint = |buf: &mut &[u8]| {
            let (mut v, mut shift) = (0u64, 0);
            loop {
                let b = buf[0];
                *buf = &buf[1..];
                v |= u64::from(b & 0x7f) << shift;
                shift += 7;
                if b < 0x80 {
                    return v;
                }
            }
        };
        while !buf.is_empty() {
            let key = varint(&mut buf);
            match key & 7 {
                0 => {
                    varint(&mut buf);
                }
                1 => buf = &buf[8..],
                2 => {
                    let len = usize::try_from(varint(&mut buf)).unwrap();
                    out.push((key >> 3, buf[..len].to_vec()));
                    buf = &buf[len..];
                }
                w => panic!("wire type {w}"),
            }
        }
        out
    }

    fn get(buf: &[u8], field: u64) -> Vec<Vec<u8>> {
        fields(buf)
            .into_iter()
            .filter(|(f, _)| *f == field)
            .map(|(_, v)| v)
            .collect()
    }

    #[test]
    fn an_attribute_is_a_key_value_message() {
        let mut buf = Vec::new();
        attr(&mut buf, 9, "a", &text("b"));
        assert_eq!(buf, [0x4a, 8, 0x0a, 1, b'a', 0x12, 3, 0x0a, 1, b'b']);
        let mut big = Vec::new();
        pb::varint(&mut big, 300);
        assert_eq!(big, [0xac, 0x02]);
    }

    #[test]
    fn a_turn_is_a_root_span_with_its_steps_below() {
        let at = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let step = |kind, name: &str, secs| Span {
            kind,
            name: name.into(),
            started_at: at + time::Duration::seconds(secs),
            ended_at: Some(at + time::Duration::seconds(secs + 1)),
            error: None,
            input: Some(serde_json::json!({"command": "ls"})),
            input_tokens: Some(10),
            output_tokens: Some(5),
            cache_read_tokens: Some(100),
            cache_write_tokens: None,
            cost_usd: None,
        };
        let trace = Trace {
            turn_id: "turn-1".into(),
            started_at: at,
            ended_at: Some(at + time::Duration::seconds(5)),
            outcome: Some("completed".into()),
            error: None,
            models_tracked: true,
            spans: vec![step(SpanKind::Model, "claude-x", 0), {
                let mut bash = step(SpanKind::Tool, "bash", 2);
                bash.error = Some("cat: /secret/notes.txt: No such file".into());
                bash
            }],
        };
        let body = encode(&TurnInfo {
            trace: &trace,
            agent_id: "a1",
            agent_name: "ada",
            project: Some("team"),
            output: Some("done"),
            content: false,
        });
        let resource_spans = get(&body, 1);
        assert_eq!(resource_spans.len(), 1);
        let resource = &get(&resource_spans[0], 1)[0];
        let keys: Vec<Vec<u8>> = get(resource, 1)
            .iter()
            .map(|kv| get(kv, 1)[0].clone())
            .collect();
        assert!(keys.contains(&b"openinference.project.name".to_vec()));
        let scope = &get(&resource_spans[0], 2)[0];
        let spans = get(scope, 2);
        assert_eq!(spans.len(), 3);
        let root_id = &get(&spans[0], 2)[0];
        assert!(get(&spans[0], 4).is_empty(), "the root has no parent");
        for child in &spans[1..] {
            assert_eq!(get(child, 1)[0].len(), 16);
            assert_eq!(&get(child, 4)[0], root_id);
        }
        let attrs: Vec<(Vec<u8>, Vec<u8>)> = get(&spans[1], 9)
            .iter()
            .map(|kv| (get(kv, 1)[0].clone(), get(kv, 2)[0].clone()))
            .collect();
        let kind = attrs
            .iter()
            .find(|(k, _)| k == b"openinference.span.kind")
            .unwrap();
        assert_eq!(get(&kind.1, 1)[0], b"LLM");
        assert_eq!(get(&spans[2], 5)[0], b"bash");
        assert!(
            !attrs.iter().any(|(k, _)| k == b"input.value"),
            "inputs stay out unless content is on"
        );
        let status = &get(&spans[2], 15)[0];
        assert_eq!(
            get(status, 2)[0],
            b"failed",
            "no tool output without content"
        );
    }

    #[test]
    fn settings_need_an_http_endpoint_and_a_plain_header_name() {
        let mut ok = OtelSettings {
            enabled: true,
            endpoint: " http://localhost:6006/v1/traces ".into(),
            header: Some(" Authorization ".into()),
            project: Some("  ".into()),
            content: false,
        };
        validate(&mut ok).unwrap();
        assert_eq!(ok.endpoint, "http://localhost:6006/v1/traces");
        assert_eq!(
            (ok.header.as_deref(), ok.project),
            (Some("Authorization"), None)
        );
        for bad in [
            OtelSettings {
                enabled: true,
                ..OtelSettings::default()
            },
            OtelSettings {
                endpoint: "file:///etc/passwd".into(),
                ..OtelSettings::default()
            },
            OtelSettings {
                endpoint: "https://x.test".into(),
                header: Some("Bad Header:".into()),
                ..OtelSettings::default()
            },
        ] {
            assert!(validate(&mut bad.clone()).is_err(), "{bad:?}");
        }
    }
}
