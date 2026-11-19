//! Operator-facing HTTP + WebSocket API used by the desktop UI.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use ferrobot_core::{Agent, AgentId, AgentName, MailMessage, Sender};
use ferrobot_sandbox::{ExecOutput, ExecRequest, SnapshotRef};
use ferrobot_store::{NewAgent, StoredEvent};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use utoipa::{IntoParams, ToSchema};

use crate::error::{ApiError, ErrorBody};
use crate::{AppState, UiEvent, lifecycle};

type ApiResult<T> = Result<Json<T>, ApiError>;

const MAX_BODY_BYTES: usize = 32 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/agents", get(list_agents).post(create_agent))
        .route("/v1/agents/{id}", get(get_agent).delete(delete_agent))
        .route(
            "/v1/agents/{id}/messages",
            get(list_messages).post(post_message),
        )
        .route("/v1/agents/{id}/events", get(list_events))
        .route("/v1/agents/{id}/interrupt", post(interrupt))
        .route("/v1/agents/{id}/stop", post(stop))
        .route("/v1/agents/{id}/wake", post(wake))
        .route("/v1/agents/{id}/snapshot", post(snapshot))
        .route("/v1/agents/{id}/exec", post(exec))
        .route("/v1/ws", get(ui_ws))
        // Remove the sandbox but keep the agent (and its disk) registered.
        .route("/v1/agents/{id}/sandbox", delete(destroy_sandbox))
}

fn parse_id(raw: &str) -> Result<AgentId, ApiError> {
    raw.parse()
        .map_err(|_| ApiError::NotFound(format!("agent {raw} not found")))
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Health {
    name: &'static str,
    version: &'static str,
    backend: &'static str,
    driver: String,
}

/// Daemon identity and configuration summary.
#[utoipa::path(get, path = "/v1/health", tag = "system",
    responses((status = 200, body = Health)))]
pub(crate) async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        name: ferrobot_core::PROJECT_NAME,
        version: env!("CARGO_PKG_VERSION"),
        backend: state.backend.kind(),
        driver: state.settings.driver.clone(),
    })
}

/// An agent plus live connection state.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct AgentView {
    #[serde(flatten)]
    agent: Agent,
    /// Whether the agent's runner is connected right now.
    connected: bool,
}

fn view(state: &AppState, agent: Agent) -> AgentView {
    AgentView {
        connected: state.hub.is_connected(agent.id),
        agent,
    }
}

/// Lists all agents.
#[utoipa::path(get, path = "/v1/agents", tag = "agents",
    responses((status = 200, body = Vec<AgentView>)))]
pub(crate) async fn list_agents(State(state): State<AppState>) -> ApiResult<Vec<AgentView>> {
    let agents = state.store.list_agents().await?;
    Ok(Json(agents.into_iter().map(|a| view(&state, a)).collect()))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct CreateAgent {
    /// 1-32 chars, `a-z0-9-`, starts with a letter.
    #[schema(example = "alice")]
    name: String,
    /// Extra system instructions for this agent.
    #[serde(default)]
    instructions: String,
}

/// Registers an agent and provisions its sandbox.
#[utoipa::path(post, path = "/v1/agents", tag = "agents", request_body = CreateAgent,
    responses(
        (status = 201, body = AgentView),
        (status = 400, description = "Invalid name or instructions", body = ErrorBody),
        (status = 409, description = "Name already taken", body = ErrorBody),
        (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn create_agent(
    State(state): State<AppState>,
    Json(req): Json<CreateAgent>,
) -> Result<(StatusCode, Json<AgentView>), ApiError> {
    let name = AgentName::parse(req.name.trim())?;
    if req.instructions.len() > MAX_BODY_BYTES {
        return Err(ApiError::BadRequest("instructions too long".into()));
    }
    let agent = state
        .store
        .create_agent(NewAgent {
            name,
            instructions: req.instructions,
        })
        .await?;
    lifecycle::provision(&state, &agent).await?;
    let agent = state.store.get_agent(agent.id).await?;
    Ok((StatusCode::CREATED, Json(view(&state, agent))))
}

/// Fetches one agent.
#[utoipa::path(get, path = "/v1/agents/{id}", tag = "agents", params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = AgentView), (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn get_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<AgentView> {
    let agent = state.store.get_agent(parse_id(&id)?).await?;
    Ok(Json(view(&state, agent)))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct DeleteQuery {
    /// Also delete the agent's persistent disk.
    #[serde(default)]
    purge: bool,
}

/// Deletes an agent and its sandbox.
#[utoipa::path(delete, path = "/v1/agents/{id}", tag = "agents", params(("id" = String, Path, description = "Agent id (UUID)"), DeleteQuery),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Unknown agent", body = ErrorBody), (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    lifecycle::delete(&state, parse_id(&id)?, q.purge).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Removes the sandbox but keeps the agent and its disk; `wake` recreates it.
#[utoipa::path(delete, path = "/v1/agents/{id}/sandbox", tag = "sandbox", params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 204, description = "Sandbox removed"), (status = 404, description = "Unknown agent", body = ErrorBody), (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn destroy_sandbox(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let agent = state.store.get_agent(parse_id(&id)?).await?;
    let handle = lifecycle::sandbox_handle(&agent)?;
    state.backend.destroy(&handle, false).await?;
    state
        .store
        .set_agent_status(agent.id, ferrobot_core::AgentStatus::Stopped)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct PostMessage {
    /// 1..32768 bytes after trimming.
    body: String,
}

/// Sends a message from the operator (`user`) to an agent's mailbox.
#[utoipa::path(post, path = "/v1/agents/{id}/messages", tag = "mailbox", params(("id" = String, Path, description = "Agent id (UUID)")),
    request_body = PostMessage,
    responses(
        (status = 202, description = "Queued for delivery", body = MailMessage),
        (status = 400, body = ErrorBody), (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn post_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PostMessage>,
) -> Result<(StatusCode, Json<MailMessage>), ApiError> {
    let id = parse_id(&id)?;
    let body = req.body.trim();
    if body.is_empty() || body.len() > MAX_BODY_BYTES {
        return Err(ApiError::BadRequest("body must be 1..32768 bytes".into()));
    }
    let msg = state
        .store
        .enqueue_message(Sender::User, id, body.to_owned(), None)
        .await?;
    state.hub.wake(id);
    state.publish(UiEvent::Message {
        message: msg.clone(),
    });
    Ok((StatusCode::ACCEPTED, Json(msg)))
}

/// The agent's conversation: messages to and from it, oldest first (last 200).
#[utoipa::path(get, path = "/v1/agents/{id}/messages", tag = "mailbox", params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = Vec<MailMessage>), (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn list_messages(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Vec<MailMessage>> {
    Ok(Json(state.store.conversation(parse_id(&id)?, 200).await?))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct EventsQuery {
    /// Return events with an id greater than this (default 0).
    #[serde(default)]
    after: i64,
}

/// Turn events (text, tool use, denials, logs, turn results), up to 500 per call.
#[utoipa::path(get, path = "/v1/agents/{id}/events", tag = "events", params(("id" = String, Path, description = "Agent id (UUID)"), EventsQuery),
    responses((status = 200, body = Vec<StoredEvent>), (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn list_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Vec<StoredEvent>> {
    Ok(Json(
        state
            .store
            .events_after(parse_id(&id)?, q.after, 500)
            .await?,
    ))
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Interrupted {
    /// The interrupted turn, or null if the agent was idle.
    turn_id: Option<String>,
}

/// Aborts the agent's running turn, if any. Interrupted messages are not retried.
#[utoipa::path(post, path = "/v1/agents/{id}/interrupt", tag = "agents", params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = Interrupted), (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn interrupt(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Interrupted> {
    let id = parse_id(&id)?;
    state.store.get_agent(id).await?;
    Ok(Json(Interrupted {
        turn_id: state.hub.interrupt(id),
    }))
}

/// Stops the agent's sandbox (disk is kept).
#[utoipa::path(post, path = "/v1/agents/{id}/stop", tag = "sandbox", params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 204, description = "Stopped"), (status = 404, description = "Unknown agent", body = ErrorBody), (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn stop(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    lifecycle::stop(&state, parse_id(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Starts (or re-creates) the agent's sandbox.
#[utoipa::path(post, path = "/v1/agents/{id}/wake", tag = "sandbox", params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 204, description = "Running"), (status = 404, description = "Unknown agent", body = ErrorBody), (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn wake(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    lifecycle::wake(&state, parse_id(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct SnapshotReq {
    /// Free-form label (default `manual`).
    #[serde(default = "default_label")]
    label: String,
}

fn default_label() -> String {
    "manual".into()
}

/// Snapshots the sandbox (backend-specific scope, see `SnapshotRef.scope`).
#[utoipa::path(post, path = "/v1/agents/{id}/snapshot", tag = "sandbox", params(("id" = String, Path, description = "Agent id (UUID)")),
    request_body = SnapshotReq,
    responses((status = 200, body = SnapshotRef), (status = 404, description = "Unknown agent", body = ErrorBody),
        (status = 501, description = "Backend cannot snapshot", body = ErrorBody), (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn snapshot(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SnapshotReq>,
) -> ApiResult<SnapshotRef> {
    let agent = state.store.get_agent(parse_id(&id)?).await?;
    let handle = lifecycle::sandbox_handle(&agent)?;
    Ok(Json(state.backend.snapshot(&handle, &req.label).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct ExecReq {
    /// argv; must not be empty.
    #[schema(example = json!(["sh", "-c", "ls"]))]
    cmd: Vec<String>,
    workdir: Option<String>,
}

/// Runs a command in the agent's computer (operator only, loopback API).
#[utoipa::path(post, path = "/v1/agents/{id}/exec", tag = "sandbox", params(("id" = String, Path, description = "Agent id (UUID)")),
    request_body = ExecReq,
    responses((status = 200, body = ExecOutput), (status = 400, body = ErrorBody), (status = 404, description = "Unknown agent", body = ErrorBody), (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn exec(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ExecReq>,
) -> ApiResult<ExecOutput> {
    if req.cmd.is_empty() {
        return Err(ApiError::BadRequest("cmd must not be empty".into()));
    }
    let agent = state.store.get_agent(parse_id(&id)?).await?;
    let handle = lifecycle::sandbox_handle(&agent)?;
    let request = ExecRequest {
        cmd: req.cmd,
        workdir: req.workdir,
        ..ExecRequest::default()
    };
    Ok(Json(state.backend.exec(&handle, &request).await?))
}

/// WebSocket stream of `UiEvent` JSON text frames (schema:
/// `protocol/ui-events.schema.json`). Client-to-server frames are ignored.
#[utoipa::path(get, path = "/v1/ws", tag = "events",
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
                    tracing::warn!(skipped = n, "UI client lagging; it should refetch");
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
