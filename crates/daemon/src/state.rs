use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ferrobot_core::{AgentId, MailMessage};
use ferrobot_sandbox::SandboxBackend;
use ferrobot_store::{Room, RoomLine, Store, StoredEvent};
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
    /// Real Anthropic API key; stays in the daemon (sandboxes use `/model`). Never logged.
    pub anthropic_api_key: Option<String>,
    /// Where the model proxy forwards to.
    pub anthropic_upstream: String,
    /// Optional model override for the runner.
    pub model: Option<String>,
    /// A turn running longer than this is interrupted and retried.
    pub turn_timeout: Duration,
    /// Pause an idle agent's sandbox (warm sleep) after this long; `None`: never.
    pub warm_timeout: Option<Duration>,
    /// Stop an idle agent's sandbox (cold sleep) after this long; `None`: never.
    pub idle_timeout: Option<Duration>,
    /// Archive an idle agent's sandbox to object storage after this long; `None`: never.
    pub archive_timeout: Option<Duration>,
    /// How often the supervisor checks idle and crashed sandboxes.
    pub supervisor_interval: Duration,
    /// Default IANA time zone for new routines.
    pub default_timezone: String,
    /// Bearer token required on the UI API (except health). `None` turns
    /// auth off; `ferrobotd` always sets one.
    pub api_token: Option<String>,
    /// Most agents this host keeps (`FERROBOT_MAX_AGENTS`), for the API and `create_agent`.
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
    /// The host's own `claude` CLI, for agents on a Claude Code provider.
    pub claude_bin: String,
    /// The agent API as the host reaches it (the host's `claude` talks to `/mcp` here).
    pub agent_url: String,
    /// One Codex home per `ChatGPT`-signed-in provider lives below this.
    pub codex_home: std::path::PathBuf,
    /// Upstream for Codex providers signed in with `ChatGPT`.
    pub chatgpt_upstream: String,
    /// The UI API as a browser on this computer reaches it; OAuth sign-ins redirect here.
    pub ui_url: String,
    /// Object storage for files agents share (`--storage-url`).
    pub storage: Arc<dyn object_store::ObjectStore>,
    /// Base URL for every voice provider instead of its public API (tests).
    pub voice_upstream: Option<String>,
    /// Discord's REST API, for the Discord bridge (another one in tests).
    pub discord_api: String,
    /// Base URL for every CLI-login host instead of the real one (tests).
    pub login_upstream: Option<String>,
    /// Directory with the database and the API token file.
    pub data_dir: std::path::PathBuf,
    /// What `--storage-url` names, for diagnostics: `local` or its scheme (`s3`, ...).
    pub storage_kind: String,
    /// The latest log lines, for diagnostics.
    pub log: crate::logs::LogRing,
    /// The checkout a debug build updates itself from; `None`: no self-update.
    #[cfg(debug_assertions)]
    pub source_update: Option<crate::update::Source>,
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
            claude_bin,
            agent_url,
            codex_home,
            chatgpt_upstream,
            ui_url,
            storage,
            voice_upstream,
            discord_api,
            login_upstream,
            data_dir,
            storage_kind,
            log,
            #[cfg(debug_assertions)]
            source_update,
        } = self;
        let redacted = |v: &Option<String>| v.as_ref().map(|_| "[REDACTED]");
        let mut s = f.debug_struct("Settings");
        s.field("sandbox_daemon_url", sandbox_daemon_url)
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
            .field("claude_bin", claude_bin)
            .field("agent_url", agent_url)
            .field("codex_home", codex_home)
            .field("chatgpt_upstream", chatgpt_upstream)
            .field("ui_url", ui_url)
            .field("storage", storage)
            .field("voice_upstream", voice_upstream)
            .field("discord_api", discord_api)
            .field("login_upstream", login_upstream)
            .field("data_dir", data_dir)
            .field("storage_kind", storage_kind)
            .field("log", log);
        #[cfg(debug_assertions)]
        s.field("source_update", source_update);
        s.finish()
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
            claude_bin: "claude".into(),
            agent_url: "http://127.0.0.1:7879".into(),
            codex_home: std::env::temp_dir().join("ferrobot-codex"),
            chatgpt_upstream: crate::codex::CHATGPT_CODEX_UPSTREAM.into(),
            ui_url: "http://127.0.0.1:7878".into(),
            storage: Arc::new(object_store::memory::InMemory::new()),
            voice_upstream: None,
            discord_api: "https://discord.com/api/v10".into(),
            login_upstream: None,
            data_dir: std::path::PathBuf::new(),
            storage_kind: "memory".into(),
            log: crate::logs::LogRing::default(),
            #[cfg(debug_assertions)]
            source_update: None,
        }
    }
}

/// Pushed to every UI WebSocket client.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    /// A turn event was recorded (same shape as `GET /v1/agents/{id}/events`).
    Event { event: StoredEvent },
    /// Live text of the assistant message being written; not persisted, the final
    /// `assistant_text` event replaces it.
    Delta {
        agent_id: AgentId,
        turn_id: String,
        text: String,
    },
    /// An agent was created or changed (profile, presence, unread count).
    Agent { agent: Box<AgentView> },
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
    /// The agent's task with a goal was set, continued or ended; refetch it.
    GoalChanged { agent_id: AgentId },
    /// Who is in the operator's voice call changed (empty: it ended). `room_id`: the call's room,
    /// once a second agent joined. `hung_up`: the agent whose hang-up this is; its goodbye may
    /// still be playing.
    Call {
        agents: Vec<AgentId>,
        room_id: Option<i64>,
        hung_up: Option<AgentId>,
    },
    /// A call room opened, gained a member or ended; or a group was created or changed.
    Room { room: Room },
    /// Something was said in a call room or group.
    RoomLine { line: RoomLine },
    /// A group was deleted.
    RoomDeleted { room_id: i64 },
    /// Whose turn it is in a call room or group; `None`: nobody's.
    RoomFloor {
        room_id: i64,
        agent: Option<AgentId>,
    },
    /// An agent called teammates without the operator (`call_agents`); the operator may join
    /// with `POST /v1/rooms/{id}/call`.
    AgentCall {
        room_id: i64,
        host: AgentId,
        topic: String,
    },
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
    /// Signs links to shared files; loaded from the secret store once.
    pub link_key: Arc<tokio::sync::OnceCell<ring::hmac::Key>>,
    /// HTML blocks from the chat, served as pages.
    pub previews: Arc<crate::files::Previews>,
    /// Connector OAuth sign-ins waiting for the browser, refresh locks.
    pub oauth: Arc<crate::oauth::OAuth>,
    /// Model lists recently fetched from providers.
    pub model_lists: Arc<crate::models::ModelCache>,
    /// Plan limits recently read from the host's Claude Code and Codex.
    pub limits: Arc<crate::limits::LimitsCache>,
    /// What speech and transcription may still spend this minute.
    pub voice: Arc<Mutex<crate::voice::Budget>>,
    /// The operator's voice call and group chats' turns (in memory: a restart ends them). Async,
    /// so a change and its room's rows in the store happen as one step.
    pub rooms: Arc<tokio::sync::Mutex<crate::call::Rooms>>,
    /// The running or last source update (in memory: the restarted daemon starts idle).
    pub update: Arc<Mutex<crate::update::UpdateInfo>>,
    /// Where the CLI image build stands.
    pub clis: Arc<crate::clis::Builds>,
    /// Webhooks' edit lock, rate limits and last deliveries.
    pub webhooks: Arc<crate::webhooks::Webhooks>,
    /// The Discord bridge's channels and the replies it owes.
    pub discord: Arc<crate::discord::Discord>,
    /// The CLI-login proxy's CA and certificates.
    pub egress: Arc<crate::egress::Egress>,
    /// Who holds the operator's control of each agent's desktop.
    pub desktop: Arc<crate::desktop::Desktop>,
    /// When this daemon started.
    pub started: std::time::Instant,
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
            previews: Arc::default(),
            oauth: Arc::default(),
            model_lists: Arc::default(),
            limits: Arc::default(),
            voice: Arc::default(),
            rooms: Arc::default(),
            update: Arc::default(),
            clis: Arc::default(),
            webhooks: Arc::default(),
            discord: Arc::default(),
            egress: Arc::default(),
            desktop: Arc::default(),
            started: std::time::Instant::now(),
        }
    }

    pub(crate) fn approvals_lock(&self) -> MutexGuard<'_, HashMap<i64, oneshot::Sender<bool>>> {
        crate::lock(&self.approvals)
    }

    /// Broadcasts to UI clients; having no subscribers is fine.
    pub fn publish(&self, event: UiEvent) {
        // Every card and reaction is published here, so webhooks and the bridge see them all.
        if let UiEvent::Event { event } = &event {
            match event.kind.as_str() {
                "question" => {
                    crate::webhooks::question(self, event);
                    crate::discord::question(self, event);
                }
                "reaction" => crate::discord::reaction(self, event),
                _ => {}
            }
        }
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
            Ok(event) => {
                self.publish(UiEvent::Event { event });
                // The agent list shows open cards.
                if kind == "question" || kind == "question_answered" {
                    self.publish_agent(agent).await;
                }
            }
            Err(e) => tracing::error!(error = %e, %agent, kind, "failed to record event"),
        }
    }

    /// Pushes the agent's current view (profile, presence, unread) to the UI.
    pub async fn publish_agent(&self, agent: AgentId) {
        match crate::views::agent_view(self, agent).await {
            Ok(agent) => self.publish(UiEvent::Agent {
                agent: Box::new(agent),
            }),
            Err(e) => tracing::debug!(error = %e, %agent, "cannot publish agent"),
        }
    }

    /// Turns `@name` mentions of existing agents into `<@id>` tokens.
    pub async fn normalize_mentions(&self, text: &str) -> String {
        let Ok(agents) = self.store.list_agents().await else {
            return text.to_owned();
        };
        // Agents remember teammates by the names they knew; a renamed one keeps its old name.
        let aliases = self.store.agent_aliases().await.unwrap_or_default();
        ferrobot_core::mention::normalize(text, |name| {
            agents
                .iter()
                .find(|a| a.name.as_str() == name)
                .map(|a| a.id)
                .or_else(|| aliases.iter().find(|(n, _)| n == name).map(|(_, id)| *id))
        })
    }

    /// Turns `<@id>` tokens back into `@current-name`, or `@name (deleted)` (for model input).
    pub async fn render_mentions(&self, text: &str) -> String {
        let agents = self.store.list_agents().await.unwrap_or_default();
        let former = self.store.former_agent_names().await.unwrap_or_default();
        ferrobot_core::mention::render(text, |id| {
            agents
                .iter()
                .find(|a| a.id == id)
                .map(|a| a.name.to_string())
                .or_else(|| {
                    former
                        .iter()
                        .find(|(f, _)| *f == id)
                        .map(|(_, name)| format!("{name} (deleted)"))
                })
        })
    }

    /// Re-reads a message and pushes its current state to UI clients.
    pub async fn publish_message(&self, id: i64) {
        match self.store.get_message(id).await {
            Ok(message) => self.publish(UiEvent::Message { message }),
            Err(e) => tracing::warn!(error = %e, message = id, "cannot publish message"),
        }
    }

    /// Wakes `agent`'s dispatch loop, and its sandbox if it sleeps.
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
        crate::agents::global_memory_changed(self);
    }

    fn event_recorded(&self, event: &StoredEvent) {
        self.publish(UiEvent::Event {
            event: event.clone(),
        });
        if event.kind == "question" {
            let (state, agent) = (self.clone(), event.agent_id);
            tokio::spawn(async move { state.publish_agent(agent).await });
        }
    }

    fn current_turn(&self, agent: AgentId) -> Option<String> {
        self.hub.current_turn(agent)
    }

    fn request_cli(
        &self,
        requester: AgentId,
        request: ferrobot_mcp::CliRequest,
    ) -> ferrobot_mcp::BoxFuture<Result<i64, String>> {
        let state = self.clone();
        Box::pin(async move {
            crate::api::clis::request_card(&state, requester, request)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn request_connector(
        &self,
        requester: AgentId,
        request: ferrobot_mcp::ConnectorRequest,
    ) -> ferrobot_mcp::BoxFuture<Result<i64, String>> {
        let state = self.clone();
        Box::pin(async move {
            crate::api::connectors::request_card(&state, requester, request)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn hang_up(&self, agent: AgentId) -> ferrobot_mcp::BoxFuture<Result<bool, String>> {
        let state = self.clone();
        Box::pin(async move { state.hang_up(agent).await })
    }

    fn in_call(&self, agent: AgentId) -> ferrobot_mcp::BoxFuture<bool> {
        let state = self.clone();
        Box::pin(async move { state.in_call(agent).await })
    }

    fn request_routine(
        &self,
        requester: AgentId,
        request: ferrobot_mcp::RoutineRequest,
    ) -> ferrobot_mcp::BoxFuture<Result<ferrobot_mcp::RoutineRequestOutcome, String>> {
        let state = self.clone();
        Box::pin(async move {
            crate::api::routines::request(&state, requester, request)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn set_agent_tools(
        &self,
        requester: AgentId,
        request: ferrobot_mcp::AgentToolsRequest,
    ) -> ferrobot_mcp::BoxFuture<Result<ferrobot_mcp::AgentToolsOutcome, String>> {
        let state = self.clone();
        Box::pin(async move {
            crate::agents::set_tools(&state, requester, request)
                .await
                .map_err(|e| e.to_string())
        })
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

    fn desktop(
        &self,
        agent: AgentId,
        action: ferrobot_mcp::DesktopAction,
    ) -> ferrobot_mcp::BoxFuture<Result<rmcp::model::CallToolResult, String>> {
        let state = self.clone();
        Box::pin(async move { crate::desktop::act(&state, agent, action).await })
    }

    fn has_voice(&self) -> ferrobot_mcp::BoxFuture<bool> {
        let state = self.clone();
        Box::pin(async move { crate::api::voice::is_ready(&state).await })
    }

    fn call_agents(
        &self,
        host: AgentId,
        guests: Vec<AgentId>,
        topic: String,
    ) -> ferrobot_mcp::BoxFuture<Result<i64, String>> {
        let state = self.clone();
        Box::pin(async move {
            // Without voice the operator could not join, so it would only be a group chat.
            if !crate::api::voice::is_ready(&state).await {
                return Err("calls need voice set up in Settings → Voice".into());
            }
            Ok(state.call_agents(host, guests, &topic).await?.id)
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

    fn request_command(
        &self,
        requester: AgentId,
        request: ferrobot_mcp::CommandRequest,
    ) -> ferrobot_mcp::BoxFuture<Result<i64, String>> {
        let state = self.clone();
        Box::pin(async move {
            crate::api::commands::request(&state, requester, request)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn has_goal(&self, agent: AgentId) -> ferrobot_mcp::BoxFuture<bool> {
        let state = self.clone();
        Box::pin(async move { crate::goal::get(&state, agent).await.is_some() })
    }

    fn finish_goal(
        &self,
        agent: AgentId,
        summary: String,
    ) -> ferrobot_mcp::BoxFuture<Result<(), String>> {
        let state = self.clone();
        Box::pin(async move { crate::goal::finish(&state, agent, summary).await })
    }

    fn has_discord(&self, agent: AgentId) -> ferrobot_mcp::BoxFuture<bool> {
        let state = self.clone();
        Box::pin(async move { !crate::discord::channels_of(&state, agent).await.is_empty() })
    }

    fn send_to_discord(
        &self,
        agent: AgentId,
        text: String,
        channel: Option<String>,
    ) -> ferrobot_mcp::BoxFuture<Result<String, String>> {
        let state = self.clone();
        Box::pin(async move { crate::discord::send(&state, agent, &text, channel).await })
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
        Box::pin(async move {
            let mut tools = crate::claude_code::computer_tools(&state, agent).await;
            tools.extend(crate::connectors::tools(&state, agent).await);
            tools
        })
    }

    fn call_connector_tool(
        &self,
        agent: AgentId,
        name: String,
        arguments: Option<rmcp::model::JsonObject>,
    ) -> ferrobot_mcp::BoxFuture<Result<rmcp::model::CallToolResult, String>> {
        let state = self.clone();
        Box::pin(async move {
            if let Some(result) =
                crate::claude_code::call_computer(&state, agent, &name, arguments.as_ref()).await
            {
                return result;
            }
            let call = crate::tool_check::Call::Connector {
                name: &name,
                args: arguments.as_ref(),
            };
            if let Err(why) = crate::tool_check::gate(&state, agent, call).await {
                return Ok(rmcp::model::CallToolResult::error(vec![
                    rmcp::model::ContentBlock::text(why),
                ]));
            }
            crate::connectors::call(&state, agent, &name, arguments).await
        })
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
