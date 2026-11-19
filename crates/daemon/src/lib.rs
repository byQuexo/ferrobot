//! The ferrobot daemon as a library, so integration tests can run it in-process.
//!
//! Two listeners with different trust levels:
//!
//! * **UI API** (`/v1/...`): operator-facing HTTP + WebSocket, bind it to
//!   loopback only. Unauthenticated in phase 0.
//! * **Agent API** (`/runner/ws`, `/mcp`): reached from sandboxes, every request
//!   is authenticated with the agent's bearer token.

pub mod api;
pub mod error;
pub mod hub;
pub mod lifecycle;
pub mod runner_ws;
pub mod spec;
pub mod state;

use axum::Router;
use axum::http::{HeaderValue, Method, header};
use tower_http::cors::CorsLayer;

pub use state::{AppState, Settings, UiEvent};

/// Router for the operator-facing UI API.
pub fn ui_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(
            [
                "tauri://localhost",
                "http://tauri.localhost",
                "http://localhost:1420",
            ]
            .map(HeaderValue::from_static),
        )
        .allow_methods([Method::GET, Method::POST, Method::DELETE])
        .allow_headers([header::CONTENT_TYPE]);
    api::router().layer(cors).with_state(state)
}

/// Router for sandbox-facing endpoints (runner WebSocket + MCP).
pub fn agent_router(state: AppState) -> Router {
    let mcp = ferrobot_mcp::router(ferrobot_mcp::McpState {
        store: state.store.clone(),
        notifier: std::sync::Arc::new(state.clone()),
    });
    Router::new()
        .merge(runner_ws::router().with_state(state))
        .nest("/mcp", mcp)
}
