//! WebSocket endpoint for agent runners and the per-agent dispatch loop.

use std::time::Duration;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use ferrobot_core::protocol::{DaemonFrame, RunnerFrame, TurnOutcome};
use ferrobot_core::{Agent, AgentId, MailMessage, PROTOCOL_VERSION, Sender};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use crate::AppState;

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const STORE_RETRY: Duration = Duration::from_secs(1);

pub fn router() -> Router<AppState> {
    Router::new().route("/runner/ws", get(upgrade))
}

async fn upgrade(State(state): State<AppState>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
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
}

async fn run_connection(state: AppState, agent: Agent, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();

    // 1. Handshake: the runner must say hello first.
    let hello = tokio::time::timeout(HELLO_TIMEOUT, stream.next()).await;
    let Ok(Some(Ok(Message::Text(text)))) = hello else {
        tracing::warn!(agent = %agent.name, "runner did not send hello in time");
        return;
    };
    let (driver, session_id) = match serde_json::from_str::<RunnerFrame>(&text) {
        Ok(RunnerFrame::Hello {
            protocol_version,
            driver,
            session_id,
            ..
        }) if protocol_version == PROTOCOL_VERSION => (driver, session_id),
        other => {
            tracing::warn!(agent = %agent.name, ?other, "bad hello, closing");
            return;
        }
    };
    if let Some(sid) = session_id.clone() {
        if agent.session_id.as_deref() != Some(&sid) {
            let _ = state.store.set_agent_session(agent.id, sid).await;
        }
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
        .record(agent.id, None, "runner_connected", json!({ "driver": driver }))
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
    let dispatcher = tokio::spawn(dispatch_loop(state.clone(), agent.id, conn_id, out_tx, done_rx));

    // 3. Read frames until the socket closes.
    while let Some(msg) = stream.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => continue,
        };
        match serde_json::from_str::<RunnerFrame>(&text) {
            Ok(RunnerFrame::Event { turn_id, event }) => {
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
                let _ = done_tx.send(Done { turn_id, outcome, error }).await;
            }
            Ok(RunnerFrame::Hello { .. }) => {
                tracing::warn!(agent = %agent.name, "duplicate hello ignored");
            }
            Err(e) => tracing::warn!(agent = %agent.name, error = %e, "unparseable runner frame"),
        }
    }

    // 4. Cleanup: anything in flight goes back to the queue.
    dispatcher.abort();
    writer.abort();
    state.hub.unregister(agent.id, conn_id);
    if let Err(e) = state.store.requeue_in_flight(agent.id).await {
        tracing::error!(error = %e, "requeue on disconnect failed");
    }
    tracing::info!(agent = %agent.name, "runner disconnected");
    state.record(agent.id, None, "runner_disconnected", json!({})).await;
    state.publish_agent(agent.id).await;
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
    }
}

/// Feeds queued messages to the runner one turn at a time.
async fn dispatch_loop(
    state: AppState,
    agent: AgentId,
    conn_id: u64,
    out: mpsc::Sender<DaemonFrame>,
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
        if !run_turn(&state, agent, conn_id, &out, &mut done_rx, msg).await {
            return;
        }
    }
}

/// Runs one turn. Returns false when the connection is gone.
async fn run_turn(
    state: &AppState,
    agent: AgentId,
    conn_id: u64,
    out: &mpsc::Sender<DaemonFrame>,
    done_rx: &mut mpsc::Receiver<Done>,
    msg: MailMessage,
) -> bool {
    let turn_id = uuid::Uuid::new_v4().to_string();
    let from = sender_name(state, msg.sender).await;
    state
        .record(
            agent,
            Some(turn_id.clone()),
            "message_in",
            json!({ "message_id": msg.id, "from": from, "body": msg.body, "attempt": msg.attempts }),
        )
        .await;
    state.hub.set_current_turn(agent, conn_id, Some(turn_id.clone()));
    let frame = DaemonFrame::Turn {
        turn_id: turn_id.clone(),
        message_id: msg.id,
        sender: msg.sender,
        sender_name: from,
        body: msg.body.clone(),
    };
    if out.send(frame).await.is_err() {
        return false; // reader cleanup requeues
    }

    let deadline = tokio::time::Instant::now() + state.settings.turn_timeout;
    let result = loop {
        match tokio::time::timeout_at(deadline, done_rx.recv()).await {
            Ok(Some(done)) if done.turn_id == turn_id => break Some(done),
            Ok(Some(stale)) => tracing::debug!(turn = %stale.turn_id, "ignoring stale turn_done"),
            Ok(None) => return false,
            Err(_) => break None,
        }
    };
    state.hub.set_current_turn(agent, conn_id, None);

    let store_result = match result {
        // An interrupt is the operator's decision: do not redeliver.
        Some(Done {
            outcome: TurnOutcome::Completed | TurnOutcome::Interrupted,
            ..
        }) => state.store.complete_message(msg.id).await.map(|()| ()),
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
    true
}
