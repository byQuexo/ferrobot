//! One turn as a timeline: its model calls and tool calls with their times, built from the
//! turn's events and the proxy's usage rows. The same spans feed the trace view.

use ferrobot_store::{StoredEvent, TurnCall};
use serde::Serialize;
use time::OffsetDateTime;
use utoipa::ToSchema;

/// Largest tool input a trace carries, so the view can label the step.
const MAX_INPUT_BYTES: usize = 4096;
/// How much of a failed tool's output becomes its error.
const MAX_ERROR_CHARS: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    Model,
    Tool,
}

/// One step of a turn.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Span {
    pub kind: SpanKind,
    /// The model id, or the tool's name.
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub started_at: OffsetDateTime,
    /// `null` while it runs.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub ended_at: Option<OffsetDateTime>,
    /// Why it failed; `null` when it did not.
    pub error: Option<String>,
    /// A tool's input when small (up to 4 KiB), for its label.
    #[schema(value_type = Option<Object>)]
    pub input: Option<serde_json::Value>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    /// What the provider said the call cost (`OpenRouter`); `null` otherwise.
    pub cost_usd: Option<f64>,
}

/// A turn and its steps, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Trace {
    pub turn_id: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub started_at: OffsetDateTime,
    /// `null` while the turn runs.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub ended_at: Option<OffsetDateTime>,
    /// `completed`, `interrupted` or `failed`; `null` while it runs.
    pub outcome: Option<String>,
    /// Why the turn failed.
    pub error: Option<String>,
    /// Whether model calls are in `spans`: `false` on Claude Code, whose calls do not pass the
    /// daemon's model proxy.
    pub models_tracked: bool,
    pub spans: Vec<Span>,
}

fn text(v: &serde_json::Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

fn span(kind: SpanKind, name: String, started_at: OffsetDateTime) -> Span {
    Span {
        kind,
        name,
        started_at,
        ended_at: None,
        error: None,
        input: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        cost_usd: None,
    }
}

/// The trace of `turn` from its events and proxied calls; `None` when it has no events.
pub fn build(
    turn: &str,
    events: &[StoredEvent],
    calls: &[TurnCall],
    models_tracked: bool,
) -> Option<Trace> {
    let started_at = events.first()?.created_at;
    let mut trace = Trace {
        turn_id: turn.to_owned(),
        started_at,
        ended_at: None,
        outcome: None,
        error: None,
        models_tracked,
        spans: Vec::new(),
    };
    // Open tool calls by id; calls without one pair with the next result without one.
    let mut open: Vec<(Option<String>, usize)> = Vec::new();
    for e in events {
        let p = &e.payload;
        match e.kind.as_str() {
            "tool_use" => {
                let mut s = span(
                    SpanKind::Tool,
                    text(&p["name"]).unwrap_or_default(),
                    e.created_at,
                );
                s.input = Some(p["input"].clone())
                    .filter(|i| !i.is_null() && i.to_string().len() <= MAX_INPUT_BYTES);
                open.push((text(&p["id"]), trace.spans.len()));
                trace.spans.push(s);
            }
            "tool_result" => {
                let id = text(&p["id"]);
                if let Some(at) = open.iter().position(|(o, _)| *o == id) {
                    let (_, i) = open.remove(at);
                    let s = &mut trace.spans[i];
                    s.ended_at = Some(e.created_at);
                    if p["is_error"].as_bool() == Some(true) {
                        let out = p["output"].as_str().unwrap_or("error");
                        s.error = Some(out.chars().take(MAX_ERROR_CHARS).collect());
                    }
                }
            }
            "tool_denied" => {
                let mut s = span(
                    SpanKind::Tool,
                    text(&p["name"]).unwrap_or_default(),
                    e.created_at,
                );
                s.ended_at = Some(e.created_at);
                s.error = Some(format!(
                    "denied: {}",
                    p["reason"].as_str().unwrap_or_default()
                ));
                trace.spans.push(s);
            }
            "turn_done" => {
                trace.ended_at = Some(e.created_at);
                trace.outcome = text(&p["outcome"]);
                trace.error = text(&p["error"]);
            }
            _ => {}
        }
    }
    // A step left open when the turn ended stopped with it.
    if let Some(end) = trace.ended_at {
        for (_, i) in open {
            trace.spans[i].ended_at = Some(end);
        }
    }
    for c in calls {
        let started = c.ended - time::Duration::milliseconds(c.latency_ms);
        let mut s = span(SpanKind::Model, c.model.clone(), started);
        s.ended_at = Some(c.ended);
        s.error = (!c.ok).then(|| "the model call failed".to_owned());
        s.input_tokens = c.tokens.input;
        s.output_tokens = c.tokens.output;
        s.cache_read_tokens = c.tokens.cache_read;
        s.cache_write_tokens = c.tokens.cache_write;
        s.cost_usd = c.cost_usd;
        trace.started_at = trace.started_at.min(started);
        trace.spans.push(s);
    }
    trace.spans.sort_by_key(|s| s.started_at);
    Some(trace)
}

#[cfg(test)]
mod tests {
    use ferrobot_core::AgentId;
    use ferrobot_store::Tokens;
    use serde_json::json;

    use super::*;

    fn at(ms: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_795_000_000).unwrap()
            + time::Duration::milliseconds(ms)
    }

    fn ev(id: i64, ms: i64, kind: &str, payload: serde_json::Value) -> StoredEvent {
        StoredEvent {
            id,
            agent_id: AgentId::new(),
            turn_id: Some("t".into()),
            kind: kind.into(),
            payload,
            created_at: at(ms),
        }
    }

    #[test]
    fn steps_line_up_by_time_with_their_outcome() {
        let events = [
            ev(1, 0, "message_in", json!({"body": "plan it"})),
            ev(
                2,
                900,
                "tool_use",
                json!({"id": "a", "name": "read_file", "input": {"path": "brief.md"}}),
            ),
            ev(
                3,
                1300,
                "tool_use",
                json!({"name": "bash", "input": {"command": "ls"}}),
            ),
            ev(
                4,
                1400,
                "tool_result",
                json!({"id": "a", "output": "ok", "is_error": false}),
            ),
            ev(
                5,
                2000,
                "tool_result",
                json!({"output": "no such dir", "is_error": true}),
            ),
            ev(
                6,
                2100,
                "tool_denied",
                json!({"name": "shell", "reason": "deleting /"}),
            ),
            ev(
                7,
                2500,
                "tool_use",
                json!({"id": "b", "name": "write_file", "input": {"content": "x".repeat(5000)}}),
            ),
            ev(
                8,
                3000,
                "turn_done",
                json!({"outcome": "interrupted", "error": null}),
            ),
        ];
        let calls = [TurnCall {
            id: 1,
            ended: at(800),
            latency_ms: 1000,
            model: "claude-sonnet-4-5".into(),
            tokens: Tokens {
                input: Some(10),
                output: Some(5),
                cache_read: Some(100),
                cache_write: None,
            },
            cost_usd: None,
            ok: true,
        }];
        let t = build("t", &events, &calls, true).unwrap();
        assert_eq!(
            (t.started_at, t.ended_at, t.outcome.as_deref()),
            (at(-200), Some(at(3000)), Some("interrupted"))
        );
        let steps: Vec<_> = t
            .spans
            .iter()
            .map(|s| {
                (
                    s.kind,
                    s.name.as_str(),
                    s.ended_at.map(|e| (e - s.started_at).whole_milliseconds()),
                    s.error.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            steps,
            [
                (SpanKind::Model, "claude-sonnet-4-5", Some(1000), None),
                (SpanKind::Tool, "read_file", Some(500), None),
                (SpanKind::Tool, "bash", Some(700), Some("no such dir")),
                (SpanKind::Tool, "shell", Some(0), Some("denied: deleting /")),
                (SpanKind::Tool, "write_file", Some(500), None),
            ]
        );
        assert_eq!(t.spans[1].input, Some(json!({"path": "brief.md"})));
        assert_eq!(t.spans[4].input, None, "a large input stays out");
        assert_eq!(t.spans[0].cache_read_tokens, Some(100));
        assert!(build("t", &[], &calls, true).is_none());
    }
}
