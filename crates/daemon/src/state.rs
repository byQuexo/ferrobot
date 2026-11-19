use std::sync::Arc;
use std::time::Duration;

use ferrobot_core::{Agent, MailMessage};
use ferrobot_sandbox::SandboxBackend;
use ferrobot_store::{Store, StoredEvent};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::hub::Hub;

/// Static daemon settings.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Agent API base URL *as seen from inside a sandbox*.
    pub sandbox_daemon_url: String,
    /// Runner model driver: `mock` or `claude`.
    pub driver: String,
    /// Passed into sandboxes when the driver is `claude`. Never logged.
    pub anthropic_api_key: Option<String>,
    /// Optional model override for the runner.
    pub model: Option<String>,
    /// A turn running longer than this is interrupted and retried.
    pub turn_timeout: Duration,
}

/// Pushed to every UI WebSocket client.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    /// A turn event was recorded (same shape as `GET /v1/agents/{id}/events`).
    Event { event: StoredEvent },
    /// An agent was created or changed status / connection.
    Agent { agent: Agent, connected: bool },
    /// An agent was deleted.
    AgentDeleted { agent_id: ferrobot_core::AgentId },
    /// A mailbox message was queued or changed status.
    Message { message: MailMessage },
}

/// Shared daemon state. Cheap to clone.
#[derive(Debug, Clone)]
pub struct AppState {
    pub store: Store,
    pub backend: Arc<dyn SandboxBackend>,
    pub hub: Hub,
    pub ui: broadcast::Sender<UiEvent>,
    pub settings: Arc<Settings>,
}

impl AppState {
    pub fn new(store: Store, backend: Arc<dyn SandboxBackend>, settings: Settings) -> Self {
        let (ui, _) = broadcast::channel(1024);
        Self {
            store,
            backend,
            hub: Hub::default(),
            ui,
            settings: Arc::new(settings),
        }
    }

    /// Broadcasts to UI clients; having no subscribers is fine.
    pub fn publish(&self, event: UiEvent) {
        let _ = self.ui.send(event);
    }

    /// Persists an event and pushes it to the UI.
    pub async fn record(
        &self,
        agent: ferrobot_core::AgentId,
        turn_id: Option<String>,
        kind: &str,
        payload: serde_json::Value,
    ) {
        match self
            .store
            .append_event(agent, turn_id, kind.to_owned(), payload)
            .await
        {
            Ok(event) => self.publish(UiEvent::Event { event }),
            Err(e) => tracing::error!(error = %e, %agent, kind, "failed to record event"),
        }
    }

    pub async fn publish_agent(&self, agent: ferrobot_core::AgentId) {
        if let Ok(agent) = self.store.get_agent(agent).await {
            let connected = self.hub.is_connected(agent.id);
            self.publish(UiEvent::Agent { agent, connected });
        }
    }
}

impl AppState {
    /// Re-reads a message and pushes its current state to UI clients.
    pub async fn publish_message(&self, id: i64) {
        match self.store.get_message(id).await {
            Ok(message) => self.publish(UiEvent::Message { message }),
            Err(e) => tracing::warn!(error = %e, message = id, "cannot publish message"),
        }
    }
}

impl ferrobot_mcp::MailboxNotifier for AppState {
    fn message_enqueued(&self, recipient: ferrobot_core::AgentId, message_id: i64) {
        self.hub.wake(recipient);
        let state = self.clone();
        tokio::spawn(async move { state.publish_message(message_id).await });
    }
}
