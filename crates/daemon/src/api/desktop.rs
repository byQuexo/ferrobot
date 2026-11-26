//! The operator's live view of an agent's desktop (`/v1/agents/{id}/screen`).

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::Response;
use ferrobot_core::AgentId;
use ferrobot_sandbox::ExecStream;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use utoipa::IntoParams;

use super::parse_id;
use crate::AppState;
use crate::error::ApiError;

/// Keeps a watched computer from going idle.
const KEEP_AWAKE: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize, IntoParams)]
pub(crate) struct ScreenQuery {
    /// `true`: the operator's mouse and keyboard reach the desktop, and the agent's `desktop`
    /// tool waits until this connection closes. Otherwise view only.
    #[serde(default)]
    control: bool,
}

/// The agent's desktop as VNC (RFB) over binary WebSocket frames, for a VNC client such as
/// noVNC. Wakes the computer and starts the desktop; nothing listens on a port in it.
#[utoipa::path(get, path = "/v1/agents/{id}/screen", tag = "sandbox",
    params(("id" = String, Path, description = "Agent id (UUID)"), ScreenQuery),
    responses((status = 101, description = "Switching protocols; then RFB in binary frames"),
        (status = 404, description = "Unknown agent", body = crate::error::ErrorBody),
        (status = 409, description = "No desktop in this computer", body = crate::error::ErrorBody)))]
pub(crate) async fn screen(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ScreenQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let id = parse_id(&id)?;
    let handle = crate::desktop::start(&state, id).await?;
    let vnc = state
        .backend
        .exec_stream(&handle, crate::desktop::vnc_command(query.control))
        .await?;
    Ok(ws
        .protocols(["binary"])
        .on_upgrade(move |socket| relay(state, id, query.control, socket, vnc)))
}

async fn relay(state: AppState, id: AgentId, control: bool, socket: WebSocket, vnc: ExecStream) {
    let _control = control.then(|| state.desktop.take(id));
    let (mut to_client, mut from_client) = socket.split();
    let ExecStream {
        mut output,
        mut input,
    } = vnc;
    let mut awake = tokio::time::interval(KEEP_AWAKE);
    loop {
        tokio::select! {
            chunk = output.next() => match chunk {
                Some(Ok(bytes)) => {
                    if to_client.send(Message::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
                _ => break,
            },
            message = from_client.next() => match message {
                Some(Ok(Message::Binary(bytes))) => {
                    if input.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            _ = awake.tick() => state.hub.touch(id),
        }
    }
    // Closing its stdin ends the VNC server.
    let _ = input.shutdown().await;
}
