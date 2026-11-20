//! Live event stream for UI clients.

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use tokio::sync::broadcast::error::RecvError;

use crate::AppState;

/// WebSocket of `UiEvent` text frames (`protocol/ui-events.schema.json`); client frames are
/// ignored. After a reconnect or a `lagged` gap, refetch over HTTP.
#[utoipa::path(get, path = "/v1/ws", tag = "chat",
    responses((status = 101, description = "Switching protocols; then one `UiEvent` per text frame")))]
pub(crate) async fn ui_ws(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| forward_ui_events(state, socket))
}

async fn forward_ui_events(state: AppState, mut socket: WebSocket) {
    let mut rx = state.ui.subscribe();
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Ok(event) => {
                    let Ok(text) = serde_json::to_string(&event) else { continue };
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "UI client lagging; closing so it refetches");
                    break;
                }
                Err(RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
}
