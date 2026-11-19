//! The ferrobot daemon as a library, so integration tests can run it in-process.
//!
//! Two listeners with different trust levels:
//!
//! * **UI API** (`/v1/...`): operator-facing HTTP + WebSocket, bind it to
//!   loopback only. Requires the operator token (see [`auth`]).
//! * **Agent API** (`/runner/ws`, `/mcp`, `/model`): reached from sandboxes,
//!   every request is authenticated with the agent's token. `/model` is the
//!   model proxy: it swaps the agent token for the real API key, which never
//!   enters a sandbox.

pub mod agents;
pub mod api;
pub mod auth;
pub mod error;
pub mod hub;
pub mod lifecycle;
pub mod proxy;
pub mod runner_ws;
pub mod scheduler;
pub mod spec;
pub mod state;
pub mod supervisor;
pub mod views;

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
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::PUT,
            Method::DELETE,
        ])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);
    api::router()
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ))
        .layer(cors)
        .with_state(state)
}

/// Router for sandbox-facing endpoints (runner WebSocket, MCP, model proxy).
pub fn agent_router(state: AppState) -> Router {
    let mcp = ferrobot_mcp::router(ferrobot_mcp::McpState {
        store: state.store.clone(),
        notifier: std::sync::Arc::new(state.clone()),
    });
    Router::new()
        .merge(runner_ws::router().with_state(state.clone()))
        .merge(proxy::router().with_state(state))
        .nest("/mcp", mcp)
}
