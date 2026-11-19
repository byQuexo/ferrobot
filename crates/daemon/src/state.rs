use std::sync::Arc;
use std::time::Duration;

use ferrobot_core::{AgentId, MailMessage};
use ferrobot_sandbox::SandboxBackend;
use ferrobot_store::{Store, StoredEvent};
use serde::Serialize;
use tokio::sync::{Notify, broadcast};

use crate::hub::Hub;
use crate::views::AgentView;

/// Static daemon settings.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Agent API base URL *as seen from inside a sandbox*.
    pub sandbox_daemon_url: String,
    /// Runner model driver: `mock` or `claude`.
    pub driver: String,
    /// Real Anthropic API key. Stays in the daemon: sandboxes reach the model
    /// through the token-authenticated proxy at `/model`. Never logged.
    pub anthropic_api_key: Option<String>,
    /// Where the model proxy forwards to.
    pub anthropic_upstream: String,
    /// Optional model override for the runner.
    pub model: Option<String>,
    /// A turn running longer than this is interrupted and retried.
    pub turn_timeout: Duration,
    /// Sleep an agent's sandbox after this long without activity (`None`: never).
    pub idle_timeout: Option<Duration>,
    /// How often the supervisor checks idle and crashed sandboxes.
    pub supervisor_interval: Duration,
    /// Default IANA time zone for new routines.
    pub default_timezone: String,
}

impl Settings {
    /// Settings for tests and embedding: mock driver, no idle sleep.
    pub fn new(sandbox_daemon_url: impl Into<String>) -> Self {
        Self {
            sandbox_daemon_url: sandbox_daemon_url.into(),
            driver: "mock".into(),
            anthropic_api_key: None,
            anthropic_upstream: "https://api.anthropic.com".into(),
            model: None,
            turn_timeout: Duration::from_secs(600),
            idle_timeout: None,
            supervisor_interval: Duration::from_secs(15),
            default_timezone: "UTC".into(),
        }
    }
}

/// Pushed to every UI WebSocket client.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    /// A turn event was recorded (same shape as `GET /v1/agents/{id}/events`).
    Event { event: StoredEvent },
    /// Live text fragment of the assistant message being written in a turn.
    /// Not persisted: the final `assistant_text` event replaces it.
    Delta {
        agent_id: AgentId,
        turn_id: String,
        text: String,
    },
    /// An agent was created or changed (profile, presence, unread count).
    Agent { agent: AgentView },
    /// An agent was deleted.
    AgentDeleted { agent_id: AgentId },
    /// A mailbox message was queued or changed status.
    Message { message: MailMessage },
    /// The agent's memory changed; refetch it.
    MemoryChanged { agent_id: AgentId },
    /// The agent's routines or their runs changed; refetch them.
    RoutinesChanged { agent_id: AgentId },
}

/// Shared daemon state. Cheap to clone.
#[derive(Debug, Clone)]
pub struct AppState {
    pub store: Store,
    pub backend: Arc<dyn SandboxBackend>,
    pub hub: Hub,
    pub ui: broadcast::Sender<UiEvent>,
    pub settings: Arc<Settings>,
    /// Woken when routines change so the scheduler re-plans.
    pub scheduler: Arc<Notify>,
    /// HTTP client for the model proxy.
    pub http: reqwest::Client,
}

impl AppState {
    pub fn new(store: Store, backend: Arc<dyn SandboxBackend>, settings: Settings) -> Self {
        let (ui, _) = broadcast::channel(4096);
        Self {
            store,
            backend,
            hub: Hub::default(),
            ui,
            settings: Arc::new(settings),
            scheduler: Arc::new(Notify::new()),
            http: reqwest::Client::new(),
        }
    }

    /// Broadcasts to UI clients; having no subscribers is fine.
    pub fn publish(&self, event: UiEvent) {
        let _ = self.ui.send(event);
    }

    /// Persists an event and pushes it to the UI.
    pub async fn record(
        &self,
        agent: AgentId,
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

    /// Pushes the agent's current view (profile, presence, unread) to the UI.
    pub async fn publish_agent(&self, agent: AgentId) {
        match crate::views::agent_view(self, agent).await {
            Ok(agent) => self.publish(UiEvent::Agent { agent }),
            Err(e) => tracing::debug!(error = %e, %agent, "cannot publish agent"),
        }
    }

    /// Re-reads a message and pushes its current state to UI clients.
    pub async fn publish_message(&self, id: i64) {
        match self.store.get_message(id).await {
            Ok(message) => self.publish(UiEvent::Message { message }),
            Err(e) => tracing::warn!(error = %e, message = id, "cannot publish message"),
        }
    }

    /// Something was queued for `agent`: wake its sandbox if it sleeps and
    /// its dispatch loop.
    pub fn message_arrived(&self, agent: AgentId) {
        self.hub.touch(agent);
        self.hub.wake(agent);
        let state = self.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::lifecycle::ensure_awake(&state, agent).await {
                tracing::warn!(%agent, error = %e, "could not wake agent for new message");
            }
        });
    }
}

impl ferrobot_mcp::MailboxNotifier for AppState {
    fn message_enqueued(&self, recipient: AgentId, message_id: i64) {
        self.message_arrived(recipient);
        let state = self.clone();
        tokio::spawn(async move { state.publish_message(message_id).await });
    }

    fn memory_changed(&self, agent: AgentId) {
        self.publish(UiEvent::MemoryChanged { agent_id: agent });
    }
}
