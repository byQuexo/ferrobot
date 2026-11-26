//! One turn as a timeline: model and tool calls from its events and the proxy's usage rows, plus
//! the cards it waited on and the messages it handed to other agents.

use std::collections::HashMap;

use ferrobot_core::question::{QuestionAnswered, approves};
use ferrobot_core::{AgentId, Sender};
use ferrobot_store::{Store, StoreError, StoredEvent, TurnCall};
use serde::Serialize;
use time::OffsetDateTime;
use utoipa::ToSchema;

/// Largest tool input a trace carries, so the view can label the step.
const MAX_INPUT_BYTES: usize = 4096;
/// How much of a failed tool's output becomes its error.
const MAX_ERROR_CHARS: usize = 300;
/// How much of a card's title labels its wait.
const MAX_TITLE_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    Model,
    Tool,
    /// The turn's card waiting for the operator, from its creation to the answer.
    Wait,
    /// A message the turn sent another agent.
    Handoff,
}

/// Another agent's turn, reached through a message.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TurnLink {
    pub agent_id: AgentId,
    /// Its current name; `null` once it is deleted.
    pub agent_name: Option<String>,
    /// `null` while no turn has taken the message (or it predates linked turns).
    pub turn_id: Option<String>,
    pub message_id: i64,
}

/// One step of a turn.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Span {
    pub kind: SpanKind,
    /// The model id, the tool's name, a wait's approval kind (`create_agent`, `tool_call`, ...;
    /// `ask_user` for a question card) or a handoff's `message`, `task` or `answer`.
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
    /// A tool's input when small (up to 4 KiB), for its label; a wait's card title as `title`.
    #[schema(value_type = Option<Object>)]
    pub input: Option<serde_json::Value>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    /// What the provider said the call cost (`OpenRouter`); `null` otherwise.
    pub cost_usd: Option<f64>,
    /// A wait's outcome: `approved`, `declined`, `answered` or `timed_out`; `null` while open.
    pub outcome: Option<String>,
    /// A handoff's recipient and its turn that took the message.
    pub link: Option<TurnLink>,
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
    /// Whether model calls are in `spans`: from the model proxy, or on Claude Code from its own
    /// stream (`model_call` events); `false` for Claude Code turns from before those.
    pub models_tracked: bool,
    /// The other agent's turn that sent the message this turn started on (a message, a task
    /// or a task's answer); `null` when the operator or a routine started it.
    pub from: Option<TurnLink>,
    pub spans: Vec<Span>,
}

/// What a turn links to beyond its own events, looked up by [`links`].
#[derive(Debug, Default)]
pub struct Links {
    /// Cards the turn raised, with their answers.
    pub cards: Vec<(StoredEvent, Option<StoredEvent>)>,
    /// The recipient side of each handoff, by message id.
    pub handoffs: HashMap<i64, TurnLink>,
    pub from: Option<TurnLink>,
}

async fn link(store: &Store, agent: AgentId, turn_id: Option<String>, message_id: i64) -> TurnLink {
    TurnLink {
        agent_id: agent,
        agent_name: store
            .get_agent(agent)
            .await
            .ok()
            .map(|a| a.name.to_string()),
        turn_id,
        message_id,
    }
}

/// Looks up the cards `turn` raised, the turns its handoffs reached and the turn that sent the
/// message it started on.
pub async fn links(store: &Store, turn: &str, events: &[StoredEvent]) -> Result<Links, StoreError> {
    let Some(first) = events.first() else {
        return Ok(Links::default());
    };
    let mut links = Links {
        cards: store.turn_cards(turn.to_owned(), first.id).await?,
        ..Links::default()
    };
    let agent = |v: &serde_json::Value| serde_json::from_value::<AgentId>(v.clone()).ok();
    let p = &first.payload;
    if first.kind == "message_in"
        && let (Some(id), Ok(Sender::Agent(sender))) = (
            p["message_id"].as_i64(),
            serde_json::from_value::<Sender>(p["sender"].clone()),
        )
    {
        let sent_in = store.turn_with(sender, "handoff", id).await?;
        links.from = Some(link(store, sender, sent_in, id).await);
    }
    for e in events.iter().filter(|e| e.kind == "handoff") {
        if let (Some(id), Some(to)) = (e.payload["message_id"].as_i64(), agent(&e.payload["to"])) {
            let taken_in = store.turn_with(to, "message_in", id).await?;
            links
                .handoffs
                .insert(id, link(store, to, taken_in, id).await);
        }
    }
    Ok(links)
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
        outcome: None,
        link: None,
    }
}

/// A card's wait: until its answer, else until the turn's end.
fn wait(question: &StoredEvent, answer: Option<&StoredEvent>, end: Option<OffsetDateTime>) -> Span {
    let p = &question.payload;
    let approval = &p["approval"];
    let mut s = span(
        SpanKind::Wait,
        text(&approval["kind"]).unwrap_or_else(|| "ask_user".into()),
        question.created_at,
    );
    s.input = text(&p["card"]["title"])
        .or_else(|| text(&p["card"]["questions"][0]["prompt"]))
        .map(
            |t| serde_json::json!({ "title": t.chars().take(MAX_TITLE_CHARS).collect::<String>() }),
        );
    s.ended_at = answer.map(|a| a.created_at).or(end);
    s.outcome = answer
        .and_then(|a| serde_json::from_value::<QuestionAnswered>(a.payload.clone()).ok())
        .map(|a| {
            if a.timed_out {
                "timed_out"
            } else if approval.is_null() {
                "answered"
            } else if approves(&a.answers) {
                "approved"
            } else {
                "declined"
            }
            .to_owned()
        });
    s
}

/// A message the turn handed to another agent: a mark when it was sent.
fn handoff(e: &StoredEvent, links: &Links) -> Span {
    let p = &e.payload;
    let name = if p["answer"] == true {
        "answer"
    } else if p["task"] == true {
        "task"
    } else {
        "message"
    };
    let mut s = span(SpanKind::Handoff, name.into(), e.created_at);
    s.ended_at = Some(e.created_at);
    s.link = p["message_id"]
        .as_i64()
        .and_then(|id| links.handoffs.get(&id).cloned());
    s
}

/// Claude Code's own model call, read from its stream (it skips the proxy).
fn streamed(e: &StoredEvent) -> Span {
    let p = &e.payload;
    let at = |k: &str| {
        p[k].as_str()
            .and_then(|t| {
                OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339).ok()
            })
            .unwrap_or(e.created_at)
    };
    let mut s = span(
        SpanKind::Model,
        text(&p["model"]).unwrap_or_else(|| "model".into()),
        at("started_at"),
    );
    s.ended_at = Some(at("ended_at"));
    s.input_tokens = p["input_tokens"].as_i64();
    s.output_tokens = p["output_tokens"].as_i64();
    s.cache_read_tokens = p["cache_read_tokens"].as_i64();
    s.cache_write_tokens = p["cache_write_tokens"].as_i64();
    s
}

/// The trace of `turn` from its events and proxied calls; `None` when it has no events.
pub fn build(
    turn: &str,
    events: &[StoredEvent],
    calls: &[TurnCall],
    models_tracked: bool,
    links: &Links,
) -> Option<Trace> {
    let started_at = events.first()?.created_at;
    let mut trace = Trace {
        turn_id: turn.to_owned(),
        started_at,
        ended_at: None,
        outcome: None,
        error: None,
        models_tracked,
        from: links.from.clone(),
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
            "handoff" => trace.spans.push(handoff(e, links)),
            "model_call" => {
                let s = streamed(e);
                trace.started_at = trace.started_at.min(s.started_at);
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
    for (question, answer) in &links.cards {
        trace
            .spans
            .push(wait(question, answer.as_ref(), trace.ended_at));
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
        let t = build("t", &events, &calls, true, &Links::default()).unwrap();
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
        assert!(build("t", &[], &calls, true, &Links::default()).is_none());
    }

    #[test]
    fn claude_codes_model_calls_come_from_its_stream() {
        let events = [
            ev(1, 0, "message_in", json!({})),
            ev(
                2,
                1500,
                "model_call",
                json!({"model": "claude-opus", "started_at": "2026-11-18T11:06:40.100Z", "ended_at": "2026-11-18T11:06:41.500Z", "input_tokens": 3, "cache_read_tokens": 900}),
            ),
            ev(3, 2000, "turn_done", json!({"outcome": "completed"})),
        ];
        let t = build("t", &events, &[], true, &Links::default()).unwrap();
        let s = &t.spans[0];
        assert_eq!(
            (s.kind, s.name.as_str(), s.cache_read_tokens),
            (SpanKind::Model, "claude-opus", Some(900))
        );
        assert_eq!(
            s.ended_at.map(|e| (e - s.started_at).whole_milliseconds()),
            Some(1400)
        );
    }

    #[test]
    fn cards_are_waits_until_their_answer_or_the_turns_end() {
        let card = |id, ms, approval: serde_json::Value| {
            ev(
                id,
                ms,
                "question",
                json!({"version": 1, "approval": approval, "card": {"title": null, "questions": [{"prompt": "Which repo?"}]}}),
            )
        };
        let answer = |id, ms, question, value: &str, timed_out: bool| {
            ev(
                id,
                ms,
                "question_answered",
                json!({"question": question, "answers": [{"question_id": "q1", "values": [value]}], "message_id": null, "timed_out": timed_out}),
            )
        };
        let mut approval = card(10, 100, json!({"kind": "create_agent"}));
        approval.payload["card"]["title"] = json!("ada wants to create an agent");
        let tool_call = json!({"kind": "tool_call"});
        let links = Links {
            cards: vec![
                (approval, Some(answer(11, 600, 10, "approve", false))),
                (
                    card(12, 700, tool_call.clone()),
                    Some(answer(13, 900, 12, "decline", false)),
                ),
                (
                    card(14, 1000, tool_call),
                    Some(answer(15, 1500, 14, "decline", true)),
                ),
                (
                    card(16, 1600, serde_json::Value::Null),
                    Some(answer(17, 5000, 16, "main", false)),
                ),
                (card(18, 1700, json!({"kind": "create_routine"})), None),
            ],
            ..Links::default()
        };
        let events = [
            ev(1, 0, "message_in", json!({})),
            ev(2, 2000, "turn_done", json!({"outcome": "completed"})),
        ];
        let t = build("t", &events, &[], true, &links).unwrap();
        let waits: Vec<_> = t
            .spans
            .iter()
            .map(|s| {
                (
                    s.kind,
                    s.name.as_str(),
                    s.ended_at.map(|e| (e - s.started_at).whole_milliseconds()),
                    s.outcome.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            waits,
            [
                (SpanKind::Wait, "create_agent", Some(500), Some("approved")),
                (SpanKind::Wait, "tool_call", Some(200), Some("declined")),
                (SpanKind::Wait, "tool_call", Some(500), Some("timed_out")),
                (SpanKind::Wait, "ask_user", Some(3400), Some("answered")),
                (SpanKind::Wait, "create_routine", Some(300), None),
            ]
        );
        assert_eq!(
            t.spans[0].input,
            Some(json!({"title": "ada wants to create an agent"}))
        );
        assert_eq!(t.spans[3].input, Some(json!({"title": "Which repo?"})));
        assert!(
            t.spans.iter().all(|s| s.error.is_none()),
            "a decline is no error"
        );

        let running = build("t", &events[..1], &[], true, &links).unwrap();
        assert_eq!(running.spans[4].ended_at, None, "open while the turn runs");
    }

    #[test]
    fn handoffs_link_the_turn_that_took_the_message() {
        let (scout, ada) = (AgentId::new(), AgentId::new());
        let to = |agent, turn: Option<&str>, message_id| TurnLink {
            agent_id: agent,
            agent_name: Some("scout".into()),
            turn_id: turn.map(Into::into),
            message_id,
        };
        let links = Links {
            handoffs: HashMap::from([(7, to(scout, Some("s1"), 7)), (8, to(scout, None, 8))]),
            from: Some(to(ada, Some("a0"), 3)),
            ..Links::default()
        };
        let events = [
            ev(1, 0, "message_in", json!({"message_id": 3})),
            ev(
                2,
                100,
                "handoff",
                json!({"message_id": 7, "to": scout, "task": true, "answer": false}),
            ),
            ev(
                3,
                200,
                "handoff",
                json!({"message_id": 8, "to": scout, "task": false, "answer": false}),
            ),
            ev(4, 300, "turn_done", json!({"outcome": "completed"})),
            ev(
                5,
                310,
                "handoff",
                json!({"message_id": 9, "to": ada, "answer": true}),
            ),
        ];
        let t = build("t", &events, &[], true, &links).unwrap();
        assert_eq!(t.from, links.from);
        let handoffs: Vec<_> = t
            .spans
            .iter()
            .map(|s| {
                (
                    s.kind,
                    s.name.as_str(),
                    s.ended_at == Some(s.started_at),
                    s.link.as_ref().and_then(|l| l.turn_id.as_deref()),
                )
            })
            .collect();
        assert_eq!(
            handoffs,
            [
                (SpanKind::Handoff, "task", true, Some("s1")),
                (SpanKind::Handoff, "message", true, None),
                (SpanKind::Handoff, "answer", true, None),
            ]
        );
        assert_eq!(t.spans[1].link, Some(to(scout, None, 8)), "not taken yet");
        assert_eq!(t.spans[2].link, None, "no lookup for it");
    }
}
