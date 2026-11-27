//! The daemon as a library: the UI API (`/v1`, loopback, operator token) and the agent API
//! (`/runner/ws`, `/mcp`, `/model`, agent tokens; `/model` swaps in a key no sandbox sees).
//! Incoming webhooks (`/hooks/...`) share the UI listener, authorized by their own secret only.

pub mod agents;
pub mod api;
pub mod auth;
pub mod call;
pub mod claude_code;
pub mod clis;
pub mod codex;
pub mod connectors;
pub mod credentials;
pub mod desktop;
pub mod discord;
pub mod egress;
pub mod error;
pub mod files;
pub mod hub;
pub mod lifecycle;
pub mod limits;
pub mod logins;
pub mod logs;
pub mod models;
pub mod no_tools;
pub mod oauth;
pub mod otel;
pub mod pricing;
pub mod proxy;
pub mod runner_ws;
pub mod scheduler;
pub mod secrets;
pub mod signin;
pub mod spec;
pub mod state;
pub mod stop;
pub mod storage;
pub mod supervisor;
pub mod tasks;
pub mod tool_check;
pub mod trace;
pub mod update;
pub mod usage;
pub mod views;
pub mod voice;
pub mod webhooks;

use axum::Router;
use axum::http::{HeaderValue, Method, header};
use tower_http::cors::CorsLayer;

pub use state::{AppState, Settings, UiEvent};

/// A poisoned lock only means another task panicked mid-update of plain data; keep serving.
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
        .merge(webhooks::router())
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
        .merge(proxy::router().with_state(state.clone()))
        .merge(claude_code::router().with_state(state.clone()))
        .merge(tool_check::router().with_state(state.clone()))
        .nest("/mcp", mcp)
        .layer(axum::middleware::from_fn_with_state(
            state,
            egress::intercept,
        ))
}
