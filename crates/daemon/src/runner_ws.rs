//! WebSocket endpoint for agent runners and the per-agent dispatch loop.

use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use ferrobot_core::protocol::{DaemonFrame, RunnerFrame, TurnEvent, TurnOutcome};
use ferrobot_core::{Agent, AgentId, MailMessage, PROTOCOL_VERSION, Sender};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use crate::{AppState, UiEvent};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const STORE_RETRY: Duration = Duration::from_secs(1);

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
    error: Option<String>,
    steered: Vec<i64>,
}

async fn run_connection(state: AppState, agent: Agent, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();

    // 1. Handshake: the runner must say hello first.
    let hello = tokio::time::timeout(HELLO_TIMEOUT, stream.next()).await;
    let Ok(Some(Ok(Message::Text(text)))) = hello else {
        tracing::warn!(agent = %agent.name, "runner did not send hello in time");
        return;
    };
    let (driver, session_id, steering) = match serde_json::from_str::<RunnerFrame>(&text) {
        Ok(RunnerFrame::Hello {
            protocol_version,
            driver,
            session_id,
            steering,
            ..
        }) if protocol_version == PROTOCOL_VERSION => (driver, session_id, steering),
        other => {
            tracing::warn!(agent = %agent.name, ?other, "bad hello, closing");
            return;
        }
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
        instructions: agent.instructions.clone(),
        resume_session_id: session_id.or(agent.session_id.clone()),
    };
    if send(&mut sink, &welcome).await.is_err() {
        return;
    }

    // 2. Register and start writer + dispatcher.
    let (out_tx, mut out_rx) = mpsc::channel::<DaemonFrame>(32);
    let conn_id = state.hub.register(agent.id, out_tx.clone());
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

    // 3. Read frames until the socket closes.
    while let Some(msg) = stream.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => continue,
        };
        handle_frame(&state, &agent, &done_tx, &text).await;
    }

    // 4. Cleanup: anything in flight goes back to the queue.
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
                    state.heard_in_call(agent.id, &text);
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

/// Feeds queued messages to the runner one turn at a time.
/// One runner connection, as the dispatch loop sees it.
struct Conn {
    id: u64,
    out: mpsc::Sender<DaemonFrame>,
    /// The runner's driver takes messages mid-turn.
    steering: bool,
}

async fn dispatch_loop(
    state: AppState,
    agent: AgentId,
    conn: Conn,
    mut done_rx: mpsc::Receiver<Done>,
) {
    let notify = state.hub.notify_handle(agent);
    loop {
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
        if !run_turn(&state, agent, &conn, &notify, &mut done_rx, msg).await {
            return;
        }
    }
}

/// Hands the operator's messages that arrived during a turn to that turn.
async fn steer(
    state: &AppState,
    agent: AgentId,
    turn_id: &str,
    out: &mpsc::Sender<DaemonFrame>,
    steered: &mut Vec<i64>,
) {
    loop {
        let msg = match state.store.claim_next_user_message(agent).await {
            Ok(Some(msg)) => msg,
            Ok(None) => return,
            Err(e) => {
                tracing::error!(error = %e, "claim failed");
                return;
            }
        };
        let from = sender_name(state, msg.sender).await;
        state
            .record(
                agent,
                Some(turn_id.to_owned()),
                "message_in",
                json!({ "message_id": msg.id, "from": from, "sender": msg.sender, "body": msg.body, "attempt": msg.attempts, "steered": true }),
            )
            .await;
        steered.push(msg.id);
        state.publish_message(msg.id).await;
        let frame = DaemonFrame::Steer {
            turn_id: turn_id.to_owned(),
            message_id: msg.id,
            sender: msg.sender,
            sender_name: from,
            body: state.render_mentions(&msg.body).await,
        };
        if out.send(frame).await.is_err() {
            return; // reader cleanup requeues
        }
    }
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

/// Runs one turn. Returns false when the connection is gone.
async fn run_turn(
    state: &AppState,
    agent: AgentId,
    conn: &Conn,
    notify: &tokio::sync::Notify,
    done_rx: &mut mpsc::Receiver<Done>,
    msg: MailMessage,
) -> bool {
    let (conn_id, out) = (conn.id, &conn.out);
    let turn_id = uuid::Uuid::new_v4().to_string();
    let from = sender_name(state, msg.sender).await;
    state
        .record(
            agent,
            Some(turn_id.clone()),
            "message_in",
            json!({ "message_id": msg.id, "from": from, "sender": msg.sender, "body": msg.body, "attempt": msg.attempts }),
        )
        .await;
    state
        .hub
        .set_current_turn(agent, conn_id, Some(turn_id.clone()));
    state.hub.touch(agent);
    state.publish_agent(agent).await;
    let frame = DaemonFrame::Turn {
        turn_id: turn_id.clone(),
        message_id: msg.id,
        sender: msg.sender,
        sender_name: from,
        body: state.render_mentions(&msg.body).await,
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
                steer(state, agent, &turn_id, out, &mut steered).await;
            }
        }
    };
    state.hub.set_current_turn(agent, conn_id, None);
    state.hub.touch(agent);
    settle_steered(state, steered, result.as_ref()).await;

    let store_result = match result {
        // An interrupt is the operator's decision: do not redeliver.
        Some(Done {
            outcome: TurnOutcome::Completed | TurnOutcome::Interrupted,
            ..
        }) => state.store.complete_message(msg.id).await,
        Some(Done { error, .. }) => state
            .store
            .fail_message(msg.id, error.unwrap_or_else(|| "turn failed".into()))
            .await
            .map(|_| ()),
        None => {
            tracing::warn!(%agent, turn = %turn_id, "turn timed out, interrupting");
            let _ = out.send(DaemonFrame::Interrupt { turn_id }).await;
            state
                .store
                .fail_message(msg.id, "turn timed out".into())
                .await
                .map(|_| ())
        }
    };
    if let Err(e) = store_result {
        tracing::error!(error = %e, message = msg.id, "failed to settle message");
    }
    state.publish_message(msg.id).await;
    state.publish_agent(agent).await;
    if let Sender::Routine(_) = msg.sender {
        state.publish(UiEvent::RoutinesChanged { agent_id: agent });
    }
    true
}
