//! WebSocket endpoint for agent runners and the per-agent dispatch loop.

use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use ferrobot_core::protocol::{DaemonFrame, RunnerFrame, TurnEvent, TurnOutcome};
use ferrobot_core::{Agent, AgentId, MailMessage, MailStatus, PROTOCOL_VERSION, Sender, ToolGroup};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use crate::{AppState, UiEvent, tasks};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Runners from before tool profiles; they get every built-in tool whatever the profile says.
const OLDEST_RUNNER: u32 = 1;
/// How long a refused runner is kept waiting: closed at once, it would reconnect within a second.
const REFUSED_HOLD: Duration = Duration::from_secs(30);
const STORE_RETRY: Duration = Duration::from_secs(1);

/// Opens a new agent's first turn ([`crate::Settings::greet_new_agents`]).
pub const GREETING: &str = "[ferrobot] You were just created. Begin your reply with a short \
    greeting to your operator, one or two lines: who you are and what you will do now.";

pub fn router() -> Router<AppState> {
    Router::new().route("/runner/ws", get(upgrade))
}

async fn upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let Some(token) = token else {
        return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
    };
    match state.store.agent_by_token(token).await {
        Ok(Some(agent)) => ws
            .max_message_size(4 * 1024 * 1024)
            .on_upgrade(move |socket| run_connection(state, agent, socket)),
        Ok(None) => (StatusCode::UNAUTHORIZED, "unknown token").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "token lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Completion report forwarded from the reader to the dispatch loop.
#[derive(Debug)]
struct Done {
    turn_id: String,
    outcome: TurnOutcome,
    result: Option<String>,
    error: Option<String>,
    steered: Vec<i64>,
}

async fn run_connection(state: AppState, agent: Agent, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();

    let hello = tokio::time::timeout(HELLO_TIMEOUT, stream.next()).await;
    let Ok(Some(Ok(Message::Text(text)))) = hello else {
        tracing::warn!(agent = %agent.name, "runner did not send hello in time");
        return;
    };
    let (protocol, driver, session_id, steering) = match serde_json::from_str::<RunnerFrame>(&text)
    {
        Ok(RunnerFrame::Hello {
            protocol_version,
            driver,
            session_id,
            steering,
            ..
        }) if (OLDEST_RUNNER..=PROTOCOL_VERSION).contains(&protocol_version) => {
            (protocol_version, driver, session_id, steering)
        }
        other => {
            tracing::warn!(agent = %agent.name, ?other, "bad hello, closing");
            return;
        }
    };
    let (out_tx, mut out_rx) = mpsc::channel::<DaemonFrame>(32);
    let conn_id = state.hub.register(agent.id, out_tx.clone(), protocol);
    let Some(agent) = admit(&state, agent.id, conn_id, protocol, &mut stream).await else {
        return;
    };
    if let Some(sid) = session_id.clone()
        && agent.session_id.as_deref() != Some(&sid)
    {
        let _ = state.store.set_agent_session(agent.id, sid).await;
    }
    let welcome = DaemonFrame::Welcome {
        protocol_version: PROTOCOL_VERSION,
        agent_id: agent.id,
        agent_name: agent.name.clone(),
        instructions: crate::agents::runner_instructions(&state, &agent).await,
        resume_session_id: session_id.or(agent.session_id.clone()),
        tools: agent.tools.clone(),
    };
    if send(&mut sink, &welcome).await.is_err() {
        state.hub.unregister(agent.id, conn_id);
        return;
    }

    if let Err(e) = state.store.requeue_in_flight(agent.id).await {
        tracing::error!(error = %e, "requeue failed");
    }
    tracing::info!(agent = %agent.name, %driver, "runner connected");
    state
        .record(
            agent.id,
            None,
            "runner_connected",
            json!({ "driver": driver }),
        )
        .await;
    state.publish_agent(agent.id).await;

    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if send(&mut sink, &frame).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let (done_tx, done_rx) = mpsc::channel::<Done>(8);
    let dispatcher = tokio::spawn(dispatch_loop(
        state.clone(),
        agent.id,
        Conn {
            id: conn_id,
            out: out_tx,
            steering,
        },
        done_rx,
    ));

    while let Some(msg) = stream.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => continue,
        };
        handle_frame(&state, &agent, &done_tx, &text).await;
    }

    dispatcher.abort();
    writer.abort();
    state.hub.unregister(agent.id, conn_id);
    if let Err(e) = state.store.requeue_in_flight(agent.id).await {
        tracing::error!(error = %e, "requeue on disconnect failed");
    }
    tracing::info!(agent = %agent.name, "runner disconnected");
    state
        .record(agent.id, None, "runner_disconnected", json!({}))
        .await;
    state.publish_agent(agent.id).await;
}

/// The agent as it is now, unless this runner cannot serve it: then the operator is told once
/// per streak and the line is held, so the runner does not retry at once.
async fn admit<S>(
    state: &AppState,
    id: AgentId,
    conn_id: u64,
    protocol: u32,
    stream: &mut S,
) -> Option<Agent>
where
    S: futures_util::Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    // Read after registering: a tools change either sees this runner or is seen here.
    let agent = match state.store.get_agent(id).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "agent lookup failed");
            state.hub.unregister(id, conn_id);
            return None;
        }
    };
    if protocol >= PROTOCOL_VERSION || fits_old_runner(&agent.tools) {
        state.hub.set_refused(id, false);
        return Some(agent);
    }
    state.hub.unregister(id, conn_id);
    tracing::warn!(agent = %agent.name, "runner too old for the agent's tools, refused");
    if state.hub.set_refused(id, true) {
        let reason = "Offline: its computer runs an older runner that cannot turn off shell, \
            files or web. Rebuild the runner image, then save its tools again.";
        state
            .record(id, None, "runner_refused", json!({ "reason": reason }))
            .await;
    }
    let _ = tokio::time::timeout(REFUSED_HOLD, async {
        while let Some(Ok(_)) = stream.next().await {}
    })
    .await;
    None
}

/// A runner from before tool profiles can only serve agents with every built-in tool on.
pub(crate) fn fits_old_runner(tools: &[ToolGroup]) -> bool {
    ToolGroup::ALL
        .iter()
        .filter(|g| g.is_builtin())
        .all(|g| tools.contains(g))
}

/// Handles one frame from the runner after the handshake.
async fn handle_frame(state: &AppState, agent: &Agent, done_tx: &mpsc::Sender<Done>, text: &str) {
    match serde_json::from_str::<RunnerFrame>(text) {
        Ok(RunnerFrame::Event {
            turn_id,
            event: TurnEvent::AssistantDelta { text },
        }) => {
            state.publish(UiEvent::Delta {
                agent_id: agent.id,
                turn_id,
                text,
            });
        }
        Ok(RunnerFrame::Event { turn_id, event }) => {
            state.hub.touch(agent.id);
            let event = match event {
                TurnEvent::AssistantText { text } => {
                    let text = state.normalize_mentions(&text).await;
                    state.call_text(&turn_id, &text).await;
                    TurnEvent::AssistantText { text }
                }
                other => other,
            };
            let payload = serde_json::to_value(&event).unwrap_or_default();
            let kind = payload["kind"].as_str().unwrap_or("event").to_owned();
            state.record(agent.id, Some(turn_id), &kind, payload).await;
        }
        Ok(RunnerFrame::TurnDone {
            turn_id,
            outcome,
            session_id,
            result,
            error,
            steered,
        }) => {
            if let Some(sid) = session_id {
                let _ = state.store.set_agent_session(agent.id, sid).await;
            }
            // First, so a recorded `turn_done` means the call already has the turn's line.
            state.call_turn_done(agent.id, &turn_id, outcome).await;
            state
                .record(
                    agent.id,
                    Some(turn_id.clone()),
                    "turn_done",
                    json!({ "outcome": outcome, "result": result, "error": error }),
                )
                .await;
            let _ = done_tx
                .send(Done {
                    turn_id,
                    outcome,
                    result,
                    error,
                    steered,
                })
                .await;
        }
        Ok(RunnerFrame::Hello { .. }) => {
            tracing::warn!(agent = %agent.name, "duplicate hello ignored");
        }
        Err(e) => tracing::warn!(agent = %agent.name, error = %e, "unparseable runner frame"),
    }
}

async fn send<S>(sink: &mut S, frame: &DaemonFrame) -> Result<(), ()>
where
    S: futures_util::Sink<Message> + Unpin,
{
    let text = serde_json::to_string(frame).map_err(|_| ())?;
    sink.send(Message::Text(text.into())).await.map_err(|_| ())
}

async fn sender_name(state: &AppState, sender: Sender) -> String {
    match sender {
        Sender::User => "user".to_owned(),
        Sender::Agent(id) => state
            .store
            .get_agent(id)
            .await
            .map_or_else(|_| format!("agent:{id}"), |a| a.name.to_string()),
        Sender::Routine(id) => state.store.get_routine(id).await.map_or_else(
            |_| format!("routine:{id}"),
            |r| format!("routine:{}", r.name),
        ),
    }
}

/// One runner connection, as the dispatch loop sees it.
struct Conn {
    id: u64,
    out: mpsc::Sender<DaemonFrame>,
    /// The runner's driver takes messages mid-turn.
    steering: bool,
}

/// Feeds queued messages to the runner one turn at a time.
async fn dispatch_loop(
    state: AppState,
    agent: AgentId,
    conn: Conn,
    mut done_rx: mpsc::Receiver<Done>,
) {
    let notify = state.hub.notify_handle(agent);
    hold_for_credentials(&state, agent, &notify).await;
    if greeting_due(&state, agent).await
        && !run_turn(&state, agent, &conn, &notify, &mut done_rx, None).await
    {
        return;
    }
    loop {
        hold_for_credentials(&state, agent, &notify).await;
        let msg = match state.store.claim_next_message(agent).await {
            Ok(Some(msg)) => msg,
            Ok(None) => {
                notify.notified().await;
                continue;
            }
            Err(e) => {
                tracing::error!(error = %e, "claim failed");
                tokio::time::sleep(STORE_RETRY).await;
                continue;
            }
        };
        if !run_turn(&state, agent, &conn, &notify, &mut done_rx, Some(msg)).await {
            return;
        }
    }
}

/// Waits while the agent's model credential is broken, so its messages stay queued.
async fn hold_for_credentials(state: &AppState, agent: AgentId, notify: &tokio::sync::Notify) {
    while state.hub.credential_error(agent).is_some() {
        notify.notified().await;
    }
}

/// An operator-created agent with nothing to do yet greets in a turn of its own.
async fn greeting_due(state: &AppState, agent: AgentId) -> bool {
    let store = &state.store;
    state.settings.greet_new_agents
        && matches!(store.get_agent(agent).await, Ok(a) if a.created_by.is_none())
        && matches!(store.has_event(agent, "message_in").await, Ok(false))
        && matches!(store.has_queued_messages(agent).await, Ok(false))
}

/// Hands the operator's messages that arrived during a turn to that turn: only those of the
/// turn's own call room (`None`: the chat), so a reply never lands in the wrong place.
async fn steer(
    state: &AppState,
    agent: AgentId,
    (turn_id, room): (&str, Option<i64>),
    out: &mpsc::Sender<DaemonFrame>,
    steered: &mut Vec<i64>,
) {
    loop {
        let msg = match state.store.claim_next_user_message(agent, room).await {
            Ok(Some(msg)) => msg,
            Ok(None) => return,
            Err(e) => {
                tracing::error!(error = %e, "claim failed");
                return;
            }
        };
        let Some(body) = state.turn_body(agent, &msg, turn_id).await else {
            skip(state, msg.id).await;
            continue;
        };
        let from = sender_name(state, msg.sender).await;
        let mut payload = json!({ "message_id": msg.id, "from": from, "sender": msg.sender, "body": msg.body, "attempt": msg.attempts, "steered": true });
        if let Some(room) = msg.room_id {
            payload["room_id"] = room.into();
        }
        state
            .record(agent, Some(turn_id.to_owned()), "message_in", payload)
            .await;
        steered.push(msg.id);
        state.publish_message(msg.id).await;
        let frame = DaemonFrame::Steer {
            turn_id: turn_id.to_owned(),
            message_id: msg.id,
            sender: msg.sender,
            sender_name: from,
            body: state.render_mentions(&body).await,
        };
        if out.send(frame).await.is_err() {
            return; // reader cleanup requeues
        }
    }
}

/// Settles a call room message whose turn has nothing new to hear.
async fn skip(state: &AppState, id: i64) {
    if let Err(e) = state.store.complete_message(id).await {
        tracing::error!(error = %e, message = id, "failed to settle message");
    }
    state.publish_message(id).await;
}

/// Steered messages the model saw are delivered; the rest wait for the next
/// turn (also after a failed turn, which is retried without them).
async fn settle_steered(state: &AppState, steered: Vec<i64>, done: Option<&Done>) {
    let seen: &[i64] = match done {
        Some(d) if matches!(d.outcome, TurnOutcome::Completed | TurnOutcome::Interrupted) => {
            &d.steered
        }
        _ => &[],
    };
    for id in steered {
        let settled = if seen.contains(&id) {
            state.store.complete_message(id).await
        } else {
            state.store.requeue_message(id).await
        };
        if let Err(e) = settled {
            tracing::error!(error = %e, message = id, "failed to settle steered message");
        }
        state.publish_message(id).await;
    }
}

/// Runs one turn; `None` is a new agent's greeting turn, which has no
/// message. Returns false when the connection is gone.
async fn run_turn(
    state: &AppState,
    agent: AgentId,
    conn: &Conn,
    notify: &tokio::sync::Notify,
    done_rx: &mut mpsc::Receiver<Done>,
    msg: Option<MailMessage>,
) -> bool {
    let (conn_id, out) = (conn.id, &conn.out);
    let turn_id = uuid::Uuid::new_v4().to_string();
    let mut prompt = String::new();
    if let Some(m) = &msg {
        let Some(body) = state.turn_body(agent, m, &turn_id).await else {
            skip(state, m.id).await;
            return true;
        };
        prompt = body;
    }
    let room = msg.as_ref().and_then(|m| m.room_id);
    // A room turn listens first; the greeting would make it talk about itself.
    let first = room.is_none()
        && state.settings.greet_new_agents
        && matches!(state.store.has_event(agent, "message_in").await, Ok(false));
    let (sender, from) = match &msg {
        Some(m) => (m.sender, sender_name(state, m.sender).await),
        None => (Sender::User, "ferrobot".to_owned()),
    };
    let body = msg.as_ref().map_or("", |m| m.body.as_str());
    let mut payload = json!({ "message_id": msg.as_ref().map(|m| m.id), "from": from, "sender": sender, "body": body, "attempt": msg.as_ref().map(|m| m.attempts) });
    if let Some(room) = room {
        payload["room_id"] = room.into();
    }
    state
        .record(agent, Some(turn_id.clone()), "message_in", payload)
        .await;
    state
        .hub
        .set_current_turn(agent, conn_id, Some(turn_id.clone()));
    state.hub.touch(agent);
    state.publish_agent(agent).await;
    let mut body = state.render_mentions(&prompt).await;
    if first {
        body = if body.is_empty() {
            GREETING.to_owned()
        } else {
            format!("{GREETING}\n\n{body}")
        };
    }
    let frame = DaemonFrame::Turn {
        turn_id: turn_id.clone(),
        message_id: msg.as_ref().map_or(0, |m| m.id),
        sender,
        sender_name: from,
        body,
    };
    if out.send(frame).await.is_err() {
        return false; // reader cleanup requeues
    }

    let deadline = tokio::time::Instant::now() + state.settings.turn_timeout;
    let mut steered = Vec::new();
    let result = loop {
        tokio::select! {
            done = tokio::time::timeout_at(deadline, done_rx.recv()) => match done {
                Ok(Some(report)) if report.turn_id == turn_id => break Some(report),
                Ok(Some(old)) => tracing::debug!(turn = %old.turn_id, "ignoring stale turn_done"),
                Ok(None) => return false,
                Err(_) => break None,
            },
            () = notify.notified(), if conn.steering => {
                steer(state, agent, (&turn_id, room), out, &mut steered).await;
            }
        }
    };
    state.hub.set_current_turn(agent, conn_id, None);
    state.hub.touch(agent);
    settle_steered(state, steered, result.as_ref()).await;
    if result.is_none() {
        tracing::warn!(%agent, turn = %turn_id, "turn timed out, interrupting");
        let _ = out.send(DaemonFrame::Interrupt { turn_id }).await;
    }
    if let Some(msg) = msg {
        settle(state, agent, &msg, result).await;
    }
    state.publish_agent(agent).await;
    true
}

/// Settles the turn's message. A task's end is reported first, so the
/// supervisor never sees it finished without an answer.
async fn settle(state: &AppState, agent: AgentId, msg: &MailMessage, result: Option<Done>) {
    let store_result = match result {
        // An interrupt is the operator's decision: do not redeliver.
        Some(Done {
            outcome: outcome @ (TurnOutcome::Completed | TurnOutcome::Interrupted),
            result,
            ..
        }) => {
            tasks::report(state, agent, msg, outcome, result.as_deref()).await;
            state.store.complete_message(msg.id).await
        }
        // It waits for the operator's fix instead of using up its attempts.
        _ if state.hub.credential_error(agent).is_some() => {
            state.store.requeue_message(msg.id).await
        }
        failed => {
            let error = match failed {
                Some(Done { error, .. }) => error.unwrap_or_else(|| "turn failed".into()),
                None => "turn timed out".into(),
            };
            match state.store.fail_message(msg.id, error.clone()).await {
                Ok(MailStatus::Failed) => {
                    tasks::report(state, agent, msg, TurnOutcome::Failed, Some(&error)).await;
                    Ok(())
                }
                other => other.map(|_| ()),
            }
        }
    };
    if let Err(e) = store_result {
        tracing::error!(error = %e, message = msg.id, "failed to settle message");
    }
    state.publish_message(msg.id).await;
    if let Sender::Routine(_) = msg.sender {
        state.publish(UiEvent::RoutinesChanged { agent_id: agent });
    }
}
