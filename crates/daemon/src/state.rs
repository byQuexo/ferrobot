use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use ferrobot_core::{AgentId, MailMessage};
use ferrobot_sandbox::SandboxBackend;
use ferrobot_store::{Store, StoredEvent};
use serde::Serialize;
use tokio::sync::{Notify, broadcast, oneshot};

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
    /// Pause an agent's sandbox (warm sleep) after this long without activity
    /// (`None`: never).
    pub warm_timeout: Option<Duration>,
    /// Stop an agent's sandbox (cold sleep) after this long without activity
    /// (`None`: never).
    pub idle_timeout: Option<Duration>,
    /// Move a sleeping agent's sandbox to object storage after this long
    /// without activity (`None`: never).
    pub archive_timeout: Option<Duration>,
    /// How often the supervisor checks idle and crashed sandboxes.
    pub supervisor_interval: Duration,
    /// Default IANA time zone for new routines.
    pub default_timezone: String,
    /// Bearer token required on the UI API (except health). `None` turns
    /// auth off; `ferrobotd` always sets one.
    pub api_token: Option<String>,
    /// Most agents this host keeps (`FERROBOT_MAX_AGENTS`), for the API and
    /// the `create_agent` tool alike.
    pub max_agents: usize,
    /// How long a `create_agent` approval card waits for the operator.
    pub approval_timeout: Duration,
    /// Where provider API keys live.
    pub secrets: crate::secrets::SecretStore,
    /// Prints a Google access token for Vertex providers.
    pub gcloud_token_cmd: Vec<String>,
    /// The daemon host's `codex` binary, for `ChatGPT` sign-in.
    pub codex_bin: String,
    /// One Codex home per `ChatGPT`-signed-in provider lives below this.
    pub codex_home: std::path::PathBuf,
    /// Upstream for Codex providers signed in with `ChatGPT`.
    pub chatgpt_upstream: String,
    /// The UI API as a browser on this computer reaches it; OAuth sign-ins
    /// redirect here.
    pub ui_url: String,
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
            warm_timeout: None,
            idle_timeout: None,
            archive_timeout: None,
            supervisor_interval: Duration::from_secs(15),
            default_timezone: "UTC".into(),
            api_token: None,
            max_agents: crate::agents::DEFAULT_MAX_AGENTS,
            approval_timeout: crate::agents::DEFAULT_APPROVAL_TIMEOUT,
            secrets: crate::secrets::SecretStore::default(),
            gcloud_token_cmd: ["gcloud", "auth", "print-access-token"]
                .map(String::from)
                .to_vec(),
            codex_bin: "codex".into(),
            codex_home: std::env::temp_dir().join("ferrobot-codex"),
            chatgpt_upstream: crate::codex::CHATGPT_CODEX_UPSTREAM.into(),
            ui_url: "http://127.0.0.1:7878".into(),
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
    /// The global memory changed; refetch it.
    GlobalMemoryChanged,
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
    /// Serializes question-card answers so a card is answered at most once.
    pub answers: Arc<tokio::sync::Mutex<()>>,
    /// Serializes agent creation so the agent limit holds.
    pub creating: Arc<tokio::sync::Mutex<()>>,
    /// Approval cards waiting for the operator, by `question` event id.
    pub approvals: Arc<Mutex<HashMap<i64, oneshot::Sender<bool>>>>,
    /// Cached Google access token for Vertex providers and when it was minted.
    pub gcloud_token: Arc<tokio::sync::Mutex<crate::proxy::CachedToken>>,
    /// `ChatGPT` sign-ins and cached tokens of Codex providers.
    pub codex: Arc<tokio::sync::Mutex<crate::codex::CodexState>>,
    /// Open sessions with connector MCP servers.
    pub connectors: Arc<crate::connectors::Sessions>,
    /// Connector OAuth sign-ins waiting for the browser.
    pub oauth: Arc<crate::oauth::Pending>,
}

impl AppState {
    /// # Panics
    ///
    /// If the TLS backend of the HTTP client cannot start.
    pub fn new(store: Store, backend: Arc<dyn SandboxBackend>, settings: Settings) -> Self {
        let (ui, _) = broadcast::channel(4096);
        Self {
            store,
            backend,
            hub: Hub::default(),
            ui,
            settings: Arc::new(settings),
            scheduler: Arc::new(Notify::new()),
            // A redirect would carry the provider key to another host.
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_else(|e| panic!("cannot build the HTTP client: {e}")),
            answers: Arc::default(),
            creating: Arc::default(),
            approvals: Arc::default(),
            gcloud_token: Arc::default(),
            codex: Arc::default(),
            connectors: Arc::default(),
            oauth: Arc::default(),
        }
    }

    pub(crate) fn approvals_lock(&self) -> MutexGuard<'_, HashMap<i64, oneshot::Sender<bool>>> {
        self.approvals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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
    /// Stores `@name` mentions of existing agents as `<@id>` tokens.
    pub async fn normalize_mentions(&self, text: &str) -> String {
        let Ok(agents) = self.store.list_agents().await else {
            return text.to_owned();
        };
        ferrobot_core::mention::normalize(text, |name| {
            agents
                .iter()
                .find(|a| a.name.as_str() == name)
                .map(|a| a.id)
        })
    }

    /// Turns `<@id>` tokens back into `@current-name` (for model input).
    pub async fn render_mentions(&self, text: &str) -> String {
        let agents = self.store.list_agents().await.unwrap_or_default();
        ferrobot_core::mention::render(text, |id| {
            agents
                .iter()
                .find(|a| a.id == id)
                .map(|a| a.name.to_string())
        })
    }

    pub async fn publish_message(&self, id: i64) {
        match self.store.get_message(id).await {
            Ok(message) => self.publish(UiEvent::Message { message }),
            Err(e) => tracing::warn!(error = %e, message = id, "cannot publish message"),
        }
    }

    /// Something was queued for `agent`: wake its sandbox if it sleeps and
    /// its dispatch loop.
    pub fn message_arrived(&self, agent: AgentId) {
        self.message_queued(agent, true);
    }

    /// A message was queued for `agent`: a connected runner picks it up as
    /// its next turn; with `wake`, a sleeping agent is woken for it.
    pub fn message_queued(&self, agent: AgentId, wake: bool) {
        self.hub.wake(agent);
        if !wake {
            return;
        }
        self.hub.touch(agent);
        let state = self.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::lifecycle::ensure_awake(&state, agent).await {
                tracing::warn!(%agent, error = %e, "could not wake agent for new message");
            }
        });
    }
}

impl ferrobot_mcp::MailboxNotifier for AppState {
    fn message_enqueued(&self, message: &MailMessage) {
        self.message_queued(message.recipient, message.priority != Some(false));
        let state = self.clone();
        let id = message.id;
        tokio::spawn(async move { state.publish_message(id).await });
    }

    fn memory_changed(&self, agent: AgentId) {
        self.publish(UiEvent::MemoryChanged { agent_id: agent });
    }

    fn global_memory_changed(&self) {
        self.publish(UiEvent::GlobalMemoryChanged);
    }

    fn event_recorded(&self, event: &StoredEvent) {
        self.publish(UiEvent::Event {
            event: event.clone(),
        });
    }

    fn request_agent(
        &self,
        requester: AgentId,
        request: ferrobot_mcp::AgentRequest,
    ) -> ferrobot_mcp::BoxFuture<Result<ferrobot_mcp::AgentRequestOutcome, String>> {
        let state = self.clone();
        Box::pin(async move {
            crate::agents::request(&state, requester, request.into())
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn connector_tools(&self, agent: AgentId) -> ferrobot_mcp::BoxFuture<Vec<rmcp::model::Tool>> {
        let state = self.clone();
        Box::pin(async move { crate::connectors::tools(&state, agent).await })
    }

    fn call_connector_tool(
        &self,
        agent: AgentId,
        name: String,
        arguments: Option<rmcp::model::JsonObject>,
    ) -> ferrobot_mcp::BoxFuture<Result<rmcp::model::CallToolResult, String>> {
        let state = self.clone();
        Box::pin(async move { crate::connectors::call(&state, agent, &name, arguments).await })
    }
}
