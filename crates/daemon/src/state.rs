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

/// Static daemon settings. `Debug` never shows the API key or token.
#[derive(Clone)]
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
    /// A new agent's first turn opens with a greeting to the operator; one
    /// created by the operator greets in a turn of its own.
    pub greet_new_agents: bool,
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
    /// Object storage for files agents share (`--storage-url`).
    pub storage: Arc<dyn object_store::ObjectStore>,
    /// Base URL for every voice provider instead of its public API (tests).
    pub voice_upstream: Option<String>,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Exhaustive on purpose: a new field must be classified here.
        let Self {
            sandbox_daemon_url,
            driver,
            anthropic_api_key,
            anthropic_upstream,
            model,
            turn_timeout,
            warm_timeout,
            idle_timeout,
            archive_timeout,
            supervisor_interval,
            default_timezone,
            api_token,
            max_agents,
            approval_timeout,
            greet_new_agents,
            secrets,
            gcloud_token_cmd,
            codex_bin,
            codex_home,
            chatgpt_upstream,
            ui_url,
            storage,
            voice_upstream,
        } = self;
        let redacted = |v: &Option<String>| v.as_ref().map(|_| "[REDACTED]");
        f.debug_struct("Settings")
            .field("sandbox_daemon_url", sandbox_daemon_url)
            .field("driver", driver)
            .field("anthropic_api_key", &redacted(anthropic_api_key))
            .field("anthropic_upstream", anthropic_upstream)
            .field("model", model)
            .field("turn_timeout", turn_timeout)
            .field("warm_timeout", warm_timeout)
            .field("idle_timeout", idle_timeout)
            .field("archive_timeout", archive_timeout)
            .field("supervisor_interval", supervisor_interval)
            .field("default_timezone", default_timezone)
            .field("api_token", &redacted(api_token))
            .field("max_agents", max_agents)
            .field("approval_timeout", approval_timeout)
            .field("greet_new_agents", greet_new_agents)
            .field("secrets", secrets)
            .field("gcloud_token_cmd", gcloud_token_cmd)
            .field("codex_bin", codex_bin)
            .field("codex_home", codex_home)
            .field("chatgpt_upstream", chatgpt_upstream)
            .field("ui_url", ui_url)
            .field("storage", storage)
            .field("voice_upstream", voice_upstream)
            .finish()
    }
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
            greet_new_agents: true,
            secrets: crate::secrets::SecretStore::default(),
            gcloud_token_cmd: ["gcloud", "auth", "print-access-token"]
                .map(String::from)
                .to_vec(),
            codex_bin: "codex".into(),
            codex_home: std::env::temp_dir().join("ferrobot-codex"),
            chatgpt_upstream: crate::codex::CHATGPT_CODEX_UPSTREAM.into(),
            ui_url: "http://127.0.0.1:7878".into(),
            storage: Arc::new(object_store::memory::InMemory::new()),
            voice_upstream: None,
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
    /// Who is in the operator's voice call changed (empty: it ended).
    Call { agents: Vec<AgentId> },
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
    /// Signs links to shared files; loaded from the keychain once.
    pub link_key: Arc<tokio::sync::OnceCell<ring::hmac::Key>>,
    /// Connector OAuth sign-ins waiting for the browser, refresh locks.
    pub oauth: Arc<crate::oauth::OAuth>,
    /// What speech and transcription may still spend this minute.
    pub voice: Arc<Mutex<crate::voice::Budget>>,
    /// The operator's voice call (in memory: a restart ends it).
    pub call: Arc<Mutex<crate::call::Call>>,
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
            #[allow(clippy::panic, reason = "startup: no TLS backend means no daemon")]
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
            link_key: Arc::default(),
            oauth: Arc::default(),
            voice: Arc::default(),
            call: Arc::default(),
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

    fn invite_to_call(
        &self,
        host: AgentId,
        guest: AgentId,
        topic: String,
    ) -> ferrobot_mcp::BoxFuture<Result<(), String>> {
        let state = self.clone();
        Box::pin(async move { state.invite_to_call(host, guest, &topic).await })
    }

    fn share_file(
        &self,
        agent: AgentId,
        path: String,
    ) -> ferrobot_mcp::BoxFuture<Result<ferrobot_mcp::SharedFile, String>> {
        let state = self.clone();
        Box::pin(async move { crate::files::share(&state, agent, &path).await })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_api_key_or_token() {
        let mut settings = Settings::new("http://127.0.0.1:7879");
        settings.anthropic_api_key = Some("sk-ant-very-secret".into());
        settings.api_token = Some("operator-token-secret".into());
        let shown = format!("{settings:?}");
        assert!(
            !shown.contains("very-secret") && !shown.contains("token-secret"),
            "{shown}"
        );
        assert!(shown.contains("[REDACTED]") && shown.contains("127.0.0.1:7879"));
    }
}
