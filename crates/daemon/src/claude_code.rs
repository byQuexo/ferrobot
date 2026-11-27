//! The opt-in Claude Code driver: the host's own, unmodified `claude`, whose login ferrobot never
//! reads, stores or relays. Its built-in tools are off; it reaches the sandbox only through MCP.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use ferrobot_core::{Agent, AgentId, ProviderKind, ToolGroup};
use ferrobot_sandbox::{ExecRequest, SandboxPath};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use uuid::Uuid;

use crate::AppState;
use crate::limits::{LimitWindow, PlanLimits};

const STATUS_TIMEOUT: Duration = Duration::from_secs(20);
const BASH_TIMEOUT_S: &str = "120";
const MAX_READ_CHARS: usize = 100_000;

/// Whether the host's `claude` is installed and signed in; never its account details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct ClaudeCodeStatus {
    pub installed: bool,
    /// `claude --version`, e.g. `2.1.330 (Claude Code)`.
    pub version: Option<String>,
    pub logged_in: bool,
    /// How it is signed in, e.g. `claude.ai`.
    pub auth_method: Option<String>,
}

/// Variables that would move `claude` off the user's own login, make it think it runs inside
/// another Claude Code, or hand the daemon's secrets to hooks and plugins.
fn foreign(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    ["ANTHROPIC_", "CLAUDE_CODE_", "FERROBOT_", "AWS_"]
        .iter()
        .any(|prefix| key.starts_with(prefix))
        || key == "CLAUDECODE"
}

const SETTING: &str = "claude_code";
const MAX_HOOKS: usize = 32;
const MAX_PLUGINS: usize = 32;
const MAX_COMMAND: usize = 4096;
const MAX_MATCHER: usize = 200;

/// When a hook runs. Only events that fire in a `claude -p` turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum HookEvent {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
    SessionEnd,
}

impl HookEvent {
    fn name(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
            Self::SessionEnd => "SessionEnd",
        }
    }

    fn per_tool(self) -> bool {
        matches!(self, Self::PreToolUse | Self::PostToolUse)
    }
}

/// A shell command Claude Code runs on this computer, outside the sandboxes, when `event` fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Hook {
    pub event: HookEvent,
    /// The tools it runs for, as Claude Code matches them (e.g. `mcp__ferrobot__bash`);
    /// tool events only. `null`: every tool.
    #[serde(default)]
    pub matcher: Option<String>,
    pub command: String,
}

/// Hooks and plugins for agents on Claude Code. Only the operator sets them; the user's own
/// Claude Code settings stay out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct ClaudeCodeSettings {
    pub hooks: Vec<Hook>,
    /// Installed plugins (`name@marketplace`) turned on for agents.
    pub plugins: Vec<String>,
    /// Claude Code's own subagents (its `Task` tool). They get the same ferrobot tools and
    /// still reach the sandbox only through MCP; they use more of the seat.
    pub subagents: bool,
}

/// A plugin installed in the host's Claude Code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct InstalledPlugin {
    /// `name@marketplace`.
    pub id: String,
    pub version: Option<String>,
    #[serde(skip)]
    path: PathBuf,
}

/// Trims and checks the operator's hooks and plugins.
pub fn validate(settings: &mut ClaudeCodeSettings) -> Result<(), String> {
    if settings.hooks.len() > MAX_HOOKS || settings.plugins.len() > MAX_PLUGINS {
        return Err(format!(
            "at most {MAX_HOOKS} hooks and {MAX_PLUGINS} plugins"
        ));
    }
    for hook in &mut settings.hooks {
        hook.command = hook.command.trim().to_owned();
        if hook.command.is_empty()
            || hook.command.len() > MAX_COMMAND
            || hook.command.contains('\0')
        {
            return Err(format!("a hook needs a command of 1-{MAX_COMMAND} bytes"));
        }
        hook.matcher = hook
            .matcher
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_owned);
        if let Some(matcher) = &hook.matcher {
            if !hook.event.per_tool() {
                return Err(format!("{} hooks have no tool matcher", hook.event.name()));
            }
            if matcher.len() > MAX_MATCHER || matcher.chars().any(char::is_control) {
                return Err(format!("a matcher has at most {MAX_MATCHER} characters"));
            }
        }
    }
    let mut seen = HashSet::new();
    settings.plugins.retain(|id| seen.insert(id.clone()));
    let part = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    };
    if let Some(bad) = settings
        .plugins
        .iter()
        .find(|id| id.len() > 200 || !id.split_once('@').is_some_and(|(n, m)| part(n) && part(m)))
    {
        return Err(format!("{bad} is not a plugin id (name@marketplace)"));
    }
    Ok(())
}

pub async fn load_settings(state: &AppState) -> Result<ClaudeCodeSettings, String> {
    match state.store.get_setting(SETTING).await {
        Ok(Some(raw)) => {
            serde_json::from_str(&raw).map_err(|e| format!("Claude Code settings: {e}"))
        }
        Ok(None) => Ok(ClaudeCodeSettings::default()),
        Err(e) => Err(e.to_string()),
    }
}

pub async fn save_settings(
    state: &AppState,
    settings: &ClaudeCodeSettings,
) -> Result<(), crate::error::ApiError> {
    crate::api::settings::save_json(state, SETTING, settings).await
}

/// The plugins installed in the host's Claude Code, from `claude plugin list`.
pub async fn installed_plugins(state: &AppState) -> Result<Vec<InstalledPlugin>, String> {
    let mut cmd = claude(state);
    cmd.args(["plugin", "list", "--json"]);
    let out = stdout_of(cmd)
        .await
        .ok_or("Claude Code is not installed or did not answer")?;
    let list: Vec<Value> =
        serde_json::from_str(&out).map_err(|_| "Claude Code gave no plugin list")?;
    Ok(list
        .iter()
        .filter_map(|p| {
            Some(InstalledPlugin {
                id: p["id"].as_str()?.to_owned(),
                version: p["version"].as_str().map(str::to_owned),
                path: PathBuf::from(p["installPath"].as_str()?),
            })
        })
        .collect())
}

/// The `--settings` file: the operator's hooks and nothing else, so no setting can bring back
/// built-in tools, another login or the user's own configuration.
fn settings_file(hooks: &[Hook]) -> Value {
    let mut events = serde_json::Map::new();
    for hook in hooks {
        let mut group = json!({ "hooks": [{ "type": "command", "command": hook.command }] });
        if let Some(matcher) = &hook.matcher {
            group["matcher"] = json!(matcher);
        }
        if let Value::Array(groups) = events.entry(hook.event.name()).or_insert_with(|| json!([])) {
            groups.push(group);
        }
    }
    json!({ "hooks": events })
}

/// What the operator added to every turn beyond the bare CLI.
#[derive(Debug, Default)]
struct Extensions {
    /// Folders of the operator's plugins that are still installed.
    plugins: Vec<PathBuf>,
    subagents: bool,
}

/// Turn-time hooks and extensions. Fails rather than run a turn without a hook the operator
/// may rely on as a guard.
async fn extensions(state: &AppState) -> Result<(Value, Extensions), String> {
    let settings = load_settings(state).await?;
    let plugins = if settings.plugins.is_empty() {
        Vec::new()
    } else {
        installed_plugins(state)
            .await?
            .into_iter()
            .filter(|p| settings.plugins.contains(&p.id))
            .map(|p| p.path)
            .collect()
    };
    let extensions = Extensions {
        plugins,
        subagents: settings.subagents,
    };
    Ok((settings_file(&settings.hooks), extensions))
}

fn claude(state: &AppState) -> Command {
    let path = std::env::var_os("PATH");
    let bin = crate::codex::find_bin(
        &state.settings.claude_bin,
        path.as_deref(),
        &crate::codex::install_dirs(),
    );
    let mut cmd = Command::new(bin);
    for (key, _) in std::env::vars_os() {
        if foreign(&key) {
            cmd.env_remove(key);
        }
    }
    cmd.stdin(Stdio::null()).kill_on_drop(true);
    cmd
}

async fn stdout_of(mut cmd: Command) -> Option<String> {
    let out = tokio::time::timeout(STATUS_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

pub async fn status(state: &AppState) -> ClaudeCodeStatus {
    let mut version = claude(state);
    version.arg("--version");
    let Some(version) = stdout_of(version).await.filter(|v| !v.is_empty()) else {
        return ClaudeCodeStatus {
            installed: false,
            version: None,
            logged_in: false,
            auth_method: None,
        };
    };
    let mut auth = claude(state);
    auth.args(["auth", "status"]);
    let auth = stdout_of(auth).await.unwrap_or_default();
    let (logged_in, auth_method) = login_of(&auth);
    ClaudeCodeStatus {
        installed: true,
        version: Some(version),
        logged_in,
        auth_method,
    }
}

/// Reads only the login state from `claude auth status`, which also prints the account's email.
fn login_of(auth_status: &str) -> (bool, Option<String>) {
    let auth: Value = serde_json::from_str(auth_status).unwrap_or_default();
    let method = auth["authMethod"].as_str().map(str::to_owned);
    (auth["loggedIn"] == true, method)
}

/// How long `claude` may take to compact a session (it is a model call).
const COMPACT_TIMEOUT: Duration = Duration::from_secs(180);

/// Why a session could not be compacted or measured.
#[derive(Debug)]
pub enum CompactError {
    /// No session yet, or the agent is in a turn.
    NotNow(&'static str),
    /// `claude` failed or did not answer.
    Failed(String),
}

/// Compacts the agent's session with `claude`'s own `/compact`. The daemon runs it on the
/// operator's word, never from message text, with no tools and none of the user's settings.
pub async fn compact(state: &AppState, agent: AgentId) -> Result<String, CompactError> {
    let dir = agent_dir(state, agent);
    let session: Uuid = tokio::fs::read_to_string(dir.join("session"))
        .await
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or(CompactError::NotNow(
            "there is no conversation to compact yet",
        ))?;
    let in_turn = || CompactError::NotNow("the agent is in a turn; try again when it is done");
    if state.hub.is_working(agent) {
        return Err(in_turn());
    }
    let _busy = Busy::take(agent).ok_or_else(in_turn)?;
    let mut cmd = claude(state);
    cmd.args([
        "-p",
        "--setting-sources=",
        "--tools=",
        "--strict-mcp-config",
        "--output-format=json",
        &format!("--resume={session}"),
        "/compact",
    ])
    .current_dir(&dir)
    .kill_on_drop(true);
    let failed = |why: &str| CompactError::Failed(why.to_owned());
    let out = tokio::time::timeout(COMPACT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| failed("Claude Code took too long to compact"))?
        .map_err(|e| CompactError::Failed(format!("cannot start claude: {e}")))?;
    let report: Value =
        serde_json::from_slice(&out.stdout).map_err(|_| failed("Claude Code gave no answer"))?;
    if report["is_error"] == true {
        return Err(failed(
            report["result"].as_str().unwrap_or("compacting failed"),
        ));
    }
    if let Some(id) = report["session_id"]
        .as_str()
        .and_then(|s| s.parse::<Uuid>().ok())
        && id != session
    {
        let _ = tokio::fs::write(dir.join("session"), id.to_string()).await;
    }
    // The ring would show the old size until the next turn reports the new one.
    let _ = tokio::fs::remove_file(dir.join("context.json")).await;
    let _ = tokio::fs::remove_file(dir.join(PARTS_FILE)).await;
    Ok(report["result"].as_str().unwrap_or_default().to_owned())
}

/// One answer from the host's `claude` on `model`: no tools, settings, MCP servers or session.
/// The prompt goes on stdin, so it never shows in a process list.
pub(crate) async fn ask(
    state: &AppState,
    model: &str,
    prompt: &str,
    timeout: Duration,
) -> Result<String, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut cmd = claude(state);
    cmd.args([
        "-p",
        "--setting-sources=",
        "--tools=",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--output-format=json",
        &format!("--model={model}"),
    ])
    .current_dir(dir.path())
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start claude: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
    }
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| "Claude Code took too long".to_owned())?
        .map_err(|e| e.to_string())?;
    let report: Value =
        serde_json::from_slice(&out.stdout).map_err(|_| "Claude Code gave no answer".to_owned())?;
    let text = report["result"].as_str().unwrap_or_default().to_owned();
    if report["is_error"] == true {
        return Err(text);
    }
    Ok(text)
}

/// The seat's 5-hour and weekly use from `claude`'s own `/usage` report, which makes no model
/// call. The report also names the account and its habits; only the two limit lines are kept.
pub async fn plan_limits(state: &AppState) -> Result<PlanLimits, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut cmd = claude(state);
    cmd.args([
        "-p",
        "--setting-sources=",
        "--tools=",
        "--no-session-persistence",
        "--output-format=json",
        "/usage",
    ])
    .current_dir(dir.path());
    let out = stdout_of(cmd)
        .await
        .ok_or("Claude Code is not installed or did not answer")?;
    let report: Value =
        serde_json::from_str(&out).map_err(|_| "Claude Code gave no usage report")?;
    Ok(limits_of(report["result"].as_str().unwrap_or_default()))
}

/// Reads `Current session: 18% used · resets Nov 23 at 9:39pm (Europe/Berlin)` and the
/// `Current week (all models):` line alike.
fn limits_of(report: &str) -> PlanLimits {
    let window = |prefix: &str| {
        report.lines().find_map(|line| {
            let (used, rest) = line.trim().strip_prefix(prefix)?.split_once("% used")?;
            Some(LimitWindow {
                used_percent: used.trim().parse().ok()?,
                resets: rest.split_once("resets ").map(|(_, r)| r.trim().to_owned()),
            })
        })
    };
    PlanLimits {
        five_hour: window("Current session:"),
        weekly: window("Current week (all models):"),
    }
}

fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 100
        && !model.starts_with('-')
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:-[]/".contains(c))
}

/// A new conversation under a daemon-chosen id, or the agent's last one. The sandbox never
/// names a session, so it cannot resume one of the user's own.
enum Session {
    New(Uuid),
    Resume(Uuid),
}

/// The turn's `claude` arguments. Every value goes as `--flag=value`, so none can pose as a flag.
fn turn_args(
    files: &Path,
    session: &Session,
    model: Option<&str>,
    extensions: &Extensions,
) -> Vec<String> {
    // No built-in tool may run on the host; the sandbox is reached through MCP only. Subagents
    // (`Task`) get the same tools, so they stay in the sandbox too.
    let (tools, allowed) = if extensions.subagents {
        ("--tools=Task", "--allowedTools=mcp__ferrobot,Task")
    } else {
        ("--tools=", "--allowedTools=mcp__ferrobot")
    };
    let mut args: Vec<String> = [
        "-p",
        "--output-format=stream-json",
        "--verbose",
        "--include-partial-messages",
        tools,
        "--strict-mcp-config",
        allowed,
        "--permission-mode=dontAsk",
        // Not the user's own hooks, plugins, CLAUDE.md or settings.
        "--setting-sources=",
    ]
    .map(str::to_owned)
    .to_vec();
    args.push(format!("--mcp-config={}", files.join("mcp.json").display()));
    args.push(format!(
        "--append-system-prompt-file={}",
        files.join("system.md").display()
    ));
    // The operator's hooks from Settings; plugins only as the operator turned them on.
    args.push(format!(
        "--settings={}",
        files.join("settings.json").display()
    ));
    args.extend(
        extensions
            .plugins
            .iter()
            .map(|dir| format!("--plugin-dir={}", dir.display())),
    );
    args.push(match session {
        Session::New(id) => format!("--session-id={id}"),
        Session::Resume(id) => format!("--resume={id}"),
    });
    if let Some(model) = model {
        args.push(format!("--model={model}"));
    }
    args
}

/// `claude`'s own `/context` report on the agent's session, with the turn's MCP config, system
/// prompt, plugins and model, so it counts what the turn had. A local command: no model call.
/// `--no-session-persistence` keeps the report out of the transcript the model reads, and the
/// measurement runs no hooks.
fn context_args(
    files: &Path,
    session: Uuid,
    model: Option<&str>,
    extensions: &Extensions,
) -> Vec<String> {
    let mut args: Vec<String> = turn_args(files, &Session::Resume(session), model, extensions)
        .into_iter()
        .filter(|a| {
            !a.starts_with("--output-format")
                && !a.starts_with("--settings=")
                && !matches!(a.as_str(), "--verbose" | "--include-partial-messages")
        })
        .collect();
    args.push(format!(
        "--settings={}",
        files.join("measure.json").display()
    ));
    args.extend(
        [
            "--no-session-persistence",
            "--output-format=json",
            "/context",
        ]
        .map(str::to_owned),
    );
    args
}

#[derive(Debug, Deserialize)]
struct TurnRequest {
    prompt: String,
    /// Appended to Claude Code's own system prompt.
    system: String,
    /// The runner has no session yet (a new or reset agent).
    #[serde(default)]
    new_session: bool,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/host/claude-code/turn", post(turn))
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Agents with a turn running; one at a time, so a sandbox cannot drain the seat in parallel.
static BUSY: LazyLock<Mutex<HashSet<AgentId>>> = LazyLock::new(Mutex::default);

struct Busy(AgentId);

impl Busy {
    fn take(agent: AgentId) -> Option<Self> {
        // Lazily: a `Busy` built for nothing would lock `BUSY` again in its `drop`.
        crate::lock(&BUSY).insert(agent).then(|| Self(agent))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        crate::lock(&BUSY).remove(&self.0);
    }
}

/// How full a Claude Code agent's context was at its newest model call, from the token counts
/// `claude` reports in its own stream; kept beside its session for the context meter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    /// Input plus cache read and write tokens of that call.
    pub tokens: i64,
    pub model: Option<String>,
    /// The context window `claude` named for that model, if it did.
    pub window: Option<i64>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
}

impl Context {
    /// Takes in one stream-json line: an assistant message's usage, or the result's window.
    fn read(context: &mut Option<Self>, line: &Value) {
        match line["type"].as_str() {
            Some("assistant") => {
                let message = &line["message"];
                let usage = &message["usage"];
                let tokens: i64 = [
                    "input_tokens",
                    "cache_read_input_tokens",
                    "cache_creation_input_tokens",
                ]
                .iter()
                .filter_map(|k| usage[*k].as_i64())
                .sum();
                if tokens > 0 {
                    *context = Some(Self {
                        tokens,
                        model: message["model"].as_str().map(str::to_owned),
                        window: None,
                        at: time::OffsetDateTime::now_utc(),
                    });
                }
            }
            Some("result") => {
                if let Some(c) = context.as_mut() {
                    let usage = &line["modelUsage"];
                    let of = c.model.as_deref().map(|m| &usage[m]);
                    c.window = of.and_then(|u| u["contextWindow"].as_i64());
                }
            }
            _ => {}
        }
    }
}

/// The agent's context as its newest Claude Code turn left it.
pub async fn context(state: &AppState, agent: AgentId) -> Option<Context> {
    read_context(&agent_dir(state, agent)).await
}

async fn read_context(dir: &Path) -> Option<Context> {
    let raw = tokio::fs::read(dir.join("context.json")).await.ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Where the newest turn's `/context` breakdown is kept, beside `context.json`.
const PARTS_FILE: &str = "context-parts.json";
/// How long `claude` may take for `/context` (it starts and connects to MCP, no model call).
const MEASURE_TIMEOUT: Duration = Duration::from_secs(60);

/// A `/context` breakdown and the turn's context it belongs to.
#[derive(Debug, Serialize, Deserialize)]
struct Parts {
    #[serde(with = "time::serde::rfc3339")]
    at: time::OffsetDateTime,
    breakdown: crate::context::Breakdown,
}

/// The breakdown `claude` reported after the turn that left `context`; `None` until it is in,
/// or when a newer turn has not been measured yet.
pub async fn breakdown(
    state: &AppState,
    agent: AgentId,
    context: &Context,
) -> Option<crate::context::Breakdown> {
    let raw = tokio::fs::read(agent_dir(state, agent).join(PARTS_FILE))
        .await
        .ok()?;
    let parts: Parts = serde_json::from_slice(&raw).ok()?;
    (parts.at == context.at).then_some(parts.breakdown)
}

/// Runs `/context` after a turn while its files still exist (the MCP config holds the agent's
/// token, which the daemon keeps only as a hash) and saves the breakdown for `at`'s context.
async fn measure(
    state: AppState,
    dir: PathBuf,
    config: tempfile::TempDir,
    args: Vec<String>,
    at: time::OffsetDateTime,
) {
    let breakdown = run_context(&state, &dir, &args).await;
    drop(config);
    if let Some(breakdown) = breakdown {
        save_parts(&dir, at, &breakdown).await;
    } else {
        tracing::debug!(dir = %dir.display(), "no /context report from claude");
    }
}

/// Runs `claude` with `/context` arguments and reads its report.
async fn run_context(
    state: &AppState,
    dir: &Path,
    args: &[String],
) -> Option<crate::context::Breakdown> {
    let mut cmd = claude(state);
    cmd.args(args).current_dir(dir);
    let out = tokio::time::timeout(MEASURE_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    let report: Value = serde_json::from_slice(&out.stdout).ok()?;
    if report["is_error"] == true {
        return None;
    }
    crate::context::parse_report(report["result"].as_str().unwrap_or_default())
}

/// Keeps `breakdown` for the turn that left the context of `at`, unless a newer turn has
/// finished meanwhile (its own measurement wins).
async fn save_parts(dir: &Path, at: time::OffsetDateTime, breakdown: &crate::context::Breakdown) {
    if read_context(dir).await.is_some_and(|c| c.at == at)
        && let Ok(raw) = serde_json::to_vec(&Parts {
            at,
            breakdown: breakdown.clone(),
        })
    {
        let _ = tokio::fs::write(dir.join(PARTS_FILE), raw).await;
    }
}

/// The newest turn's ferrobot instructions, kept beside the session for measuring on request.
const SYSTEM_FILE: &str = "system.md";

/// Files for a `/context` run outside a turn: no MCP server (the agent's token exists only
/// during a turn), the newest turn's instructions if kept, and no hooks.
fn on_request_files(instructions: &str) -> std::io::Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new()
        .prefix("ferrobot-claude-")
        .tempdir()?;
    for (name, text) in [
        ("mcp.json", json!({ "mcpServers": {} }).to_string()),
        ("system.md", instructions.to_owned()),
        (
            "measure.json",
            json!({ "disableAllHooks": true }).to_string(),
        ),
    ] {
        write_private(&dir.path().join(name), &text)?;
    }
    Ok(dir)
}

/// Measures the agent's context now with `claude`'s own `/context`, without its MCP tools,
/// which only a turn can reach. On the operator's word; a local command, no model call. The
/// breakdown is kept for the agent's newest turn and returned; a full one from that turn stays.
pub async fn measure_now(
    state: &AppState,
    agent: &Agent,
    model: Option<&str>,
) -> Result<crate::context::Breakdown, CompactError> {
    let dir = agent_dir(state, agent.id);
    let session: Uuid = tokio::fs::read_to_string(dir.join("session"))
        .await
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or(CompactError::NotNow(
            "there is no conversation to measure yet",
        ))?;
    let context = read_context(&dir)
        .await
        .ok_or(CompactError::NotNow("there is no turn to measure yet"))?;
    // The turn's own measurement counted everything: keep it.
    if let Some(full) = breakdown(state, agent.id, &context)
        .await
        .filter(|b| b.left_out.is_empty())
    {
        return Ok(full);
    }
    if state.hub.is_working(agent.id) {
        return Err(CompactError::NotNow(
            "the agent is in a turn; its breakdown comes when the turn ends",
        ));
    }
    let model = model.filter(|m| valid_model(m));
    let failed = |why: &str| CompactError::Failed(why.to_owned());
    let (_, extensions) = extensions(state).await.map_err(|e| failed(&e))?;
    let kept = tokio::fs::read_to_string(dir.join(SYSTEM_FILE)).await.ok();
    let files = on_request_files(kept.as_deref().unwrap_or_default())
        .map_err(|e| failed(&e.to_string()))?;
    let args = context_args(files.path(), session, model, &extensions);
    let mut breakdown = run_context(state, &dir, &args)
        .await
        .ok_or_else(|| failed("Claude Code gave no /context report"))?;
    breakdown
        .left_out
        .push(crate::context::ContextLeftOut::McpTools);
    if kept.is_none() {
        breakdown
            .left_out
            .push(crate::context::ContextLeftOut::Instructions);
    }
    save_parts(&dir, context.at, &breakdown).await;
    Ok(breakdown)
}

/// The model call whose reply is streaming: `claude`'s assistant lines carry its message id.
struct ModelCall {
    id: String,
    model: Option<String>,
    /// When the line before its first one arrived: the call began then.
    started: time::OffsetDateTime,
    ended: time::OffsetDateTime,
    usage: Value,
}

/// A running turn; dropping it (the runner hung up) kills `claude` and removes its MCP config.
struct Run {
    lines: Lines<BufReader<ChildStdout>>,
    child: Child,
    stderr: tokio::task::JoinHandle<String>,
    /// The turn's private files; taken by the `/context` measurement at the end.
    config: Option<tempfile::TempDir>,
    /// `claude`'s arguments for that measurement.
    measure_args: Vec<String>,
    dir: PathBuf,
    _busy: Busy,
    /// Where a new session's id is kept once its first turn succeeded.
    remember: Option<(PathBuf, Uuid)>,
    saw_result: bool,
    done: bool,
    context: Option<Context>,
    context_file: PathBuf,
    /// For usage and the trace: the agent's turn, when the last line came and the call
    /// streaming now.
    state: AppState,
    agent: AgentId,
    agent_name: String,
    provider_id: Option<Uuid>,
    turn: Option<String>,
    last_line: time::OffsetDateTime,
    /// When the line before the next call's `message_start` came: the request went out then.
    call_start: Option<time::OffsetDateTime>,
    call: Option<ModelCall>,
}

impl Run {
    /// Follows the model calls in `claude`'s stream; each becomes a usage row of its turn.
    async fn track_call(&mut self, line: &Value, now: time::OffsetDateTime) {
        let id = (line["type"] == "assistant")
            .then(|| line["message"]["id"].as_str())
            .flatten();
        if let (Some(c), Some(id)) = (self.call.as_mut(), id)
            && c.id == id
        {
            c.ended = now;
            c.usage = line["message"]["usage"].clone();
            return;
        }
        self.flush_call().await;
        if line["type"] == "stream_event" && line["event"]["type"] == "message_start" {
            self.call_start.get_or_insert(self.last_line);
        }
        if let Some(id) = id {
            self.call = Some(ModelCall {
                id: id.to_owned(),
                model: line["message"]["model"].as_str().map(str::to_owned),
                started: self.call_start.take().unwrap_or(self.last_line),
                ended: now,
                usage: line["message"]["usage"].clone(),
            });
        }
    }

    async fn flush_call(&mut self) {
        let Some(c) = self.call.take() else {
            return;
        };
        let u = &c.usage;
        let row = ferrobot_store::UsageRecord {
            at: c.ended,
            agent_id: self.agent,
            agent_name: self.agent_name.clone(),
            provider_id: self.provider_id,
            provider_kind: ProviderKind::ClaudeCode.as_str().into(),
            model: c.model.unwrap_or_else(|| "unknown".into()),
            tokens: ferrobot_store::Tokens {
                input: u["input_tokens"].as_i64(),
                output: u["output_tokens"].as_i64(),
                cache_read: u["cache_read_input_tokens"].as_i64(),
                cache_write: u["cache_creation_input_tokens"].as_i64(),
            },
            cost_usd: None,
            latency_ms: i64::try_from((c.ended - c.started).whole_milliseconds()).unwrap_or(0),
            status: ferrobot_store::UsageStatus::Ok,
            turn_id: self.turn.clone(),
        };
        if let Err(e) = self.state.store.insert_usage(row).await {
            tracing::warn!(error = %e, "cannot record a Claude Code call");
        }
    }
}

/// Runs one turn on the host's `claude` and streams its stream-json lines back to the runner.
async fn turn(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TurnRequest>,
) -> Response {
    let Some(token) = crate::proxy::agent_token(&headers) else {
        return refuse(StatusCode::UNAUTHORIZED, "missing agent token");
    };
    let agent = match state.store.agent_by_token(token).await {
        Ok(Some(agent)) => agent,
        Ok(None) => return refuse(StatusCode::UNAUTHORIZED, "unknown agent token"),
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let provider = crate::agents::provider_of(&state, &agent).await;
    let Some(provider) = provider.filter(|p| p.kind == ProviderKind::ClaudeCode) else {
        return refuse(
            StatusCode::FORBIDDEN,
            "this agent does not run on Claude Code",
        );
    };
    let Some(busy) = Busy::take(agent.id) else {
        return refuse(
            StatusCode::CONFLICT,
            "a Claude Code turn is already running",
        );
    };
    let model = state
        .call_model(agent.id)
        .await
        .or_else(|| agent.model.clone())
        .or(provider.default_model);
    if model.as_deref().is_some_and(|m| !valid_model(m)) {
        return refuse(StatusCode::BAD_REQUEST, "invalid model name");
    }
    let (Ok(prompt), Ok(system)) = (defuse(&req.prompt), defuse(&req.system)) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "a prompt may not start with / or !",
        );
    };
    let req = TurnRequest {
        prompt,
        system,
        ..req
    };
    match start(&state, &agent, token, &req, model.as_deref(), busy).await {
        Ok(run) => Response::builder()
            .header("content-type", "application/x-ndjson")
            .body(Body::from_stream(futures_util::stream::unfold(run, next)))
            .unwrap_or_else(|_| refuse(StatusCode::INTERNAL_SERVER_ERROR, "response")),
        Err(e) => {
            tracing::warn!(agent = %agent.name, error = %e, "Claude Code turn could not start");
            refuse(StatusCode::BAD_GATEWAY, &e)
        }
    }
}

async fn start(
    state: &AppState,
    agent: &Agent,
    token: &str,
    req: &TurnRequest,
    model: Option<&str>,
    busy: Busy,
) -> Result<Run, String> {
    // An empty, stable working directory: `claude` keys its sessions by it, and finds no project files.
    let dir = agent_dir(state, agent.id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let session_file = dir.join("session");
    let context_file = dir.join("context.json");
    let saved = tokio::fs::read_to_string(&session_file).await.ok();
    let session = match saved.and_then(|s| s.trim().parse().ok()) {
        Some(id) if !req.new_session => Session::Resume(id),
        _ => Session::New(Uuid::new_v4()),
    };
    let (remember, session_id) = match session {
        Session::New(id) => (Some((session_file, id)), id),
        Session::Resume(id) => (None, id),
    };
    let (settings, extensions) = extensions(state).await?;
    let config = write_turn_files(&state.settings.agent_url, token, &req.system, &settings)
        .map_err(|e| e.to_string())?;
    // For measuring on request later; the instructions hold no token.
    let _ = write_private(&dir.join(SYSTEM_FILE), &req.system);
    let measure_args = context_args(config.path(), session_id, model, &extensions);
    let mut cmd = claude(state);
    cmd.args(turn_args(config.path(), &session, model, &extensions))
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            "Claude Code is not installed on the daemon's computer".to_owned()
        }
        _ => format!("cannot start claude: {e}"),
    })?;
    let (Some(mut stdin), Some(stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err("claude has no stdio".into());
    };
    stdin
        .write_all(req.prompt.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    drop(stdin);
    let stderr = tokio::spawn(async move {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text).await;
        text
    });
    Ok(Run {
        lines: BufReader::new(stdout).lines(),
        child,
        stderr,
        config: Some(config),
        measure_args,
        dir,
        _busy: busy,
        remember,
        saw_result: false,
        done: false,
        context: None,
        context_file,
        state: state.clone(),
        agent: agent.id,
        agent_name: agent.name.to_string(),
        provider_id: agent.provider_id,
        turn: state.hub.current_turn(agent.id),
        last_line: time::OffsetDateTime::now_utc(),
        call_start: None,
        call: None,
    })
}

fn agent_dir(state: &AppState, agent: AgentId) -> PathBuf {
    state
        .settings
        .data_dir
        .join("claude-code")
        .join(agent.to_string())
}

/// Drops a deleted agent's working directory and session id; `claude` keeps its own transcripts.
pub async fn forget(state: &AppState, agent: AgentId) {
    let _ = tokio::fs::remove_dir_all(agent_dir(state, agent)).await;
}

/// Writes a file only the daemon's user can read.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// The agent's own token for the daemon's MCP server, its system prompt and the operator's
/// hooks, in private files rather than argv.
fn write_turn_files(
    agent_url: &str,
    token: &str,
    system: &str,
    settings: &Value,
) -> std::io::Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new()
        .prefix("ferrobot-claude-")
        .tempdir()?;
    let config = json!({ "mcpServers": { "ferrobot": {
        "type": "http",
        "url": format!("{}/mcp", agent_url.trim_end_matches('/')),
        "headers": { "Authorization": format!("Bearer {token}") },
    }}});
    for (name, text) in [
        ("mcp.json", config.to_string()),
        ("system.md", system.to_owned()),
        ("settings.json", settings.to_string()),
        // For the `/context` measurement after the turn: no hook runs for it.
        (
            "measure.json",
            json!({ "disableAllHooks": true }).to_string(),
        ),
    ] {
        let path = dir.path().join(name);
        std::fs::write(&path, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(dir)
}

/// `claude` attaches any `@path` and runs a leading `/command` before the model sees the prompt,
/// which comes from the sandbox or other agents: `@` paths get a space, a leading `/` or `!` fails.
fn defuse(prompt: &str) -> Result<String, ()> {
    let invisible = |c: char| {
        c.is_whitespace() || matches!(c, '\u{feff}' | '\u{200b}'..='\u{200f}' | '\u{2060}')
    };
    if matches!(
        prompt.trim_start_matches(invisible).chars().next(),
        Some('/' | '!')
    ) {
        return Err(());
    }
    let mut out = String::with_capacity(prompt.len());
    let mut rest = prompt;
    while let Some(at) = rest.find('@') {
        out.push_str(&rest[..=at]);
        rest = &rest[at + 1..];
        // Split only where every reader splits: `claude` keeps U+0085 inside a token.
        let token = rest
            .split(|c: char| c.is_ascii_whitespace())
            .next()
            .unwrap_or_default();
        // A trailing colon or comma is punctuation (`@bob:`), not part of a path.
        let inner =
            token.trim_end_matches([',', '.', ';', ':', '!', '?', ')', ']', '}', '"', '\'']);
        if token.starts_with(['.', '~', '"', '\'', '$', '%']) || inner.contains(['/', '\\', ':']) {
            out.push(' ');
        }
    }
    out.push_str(rest);
    Ok(out)
}

async fn next(mut run: Run) -> Option<(Result<Bytes, std::io::Error>, Run)> {
    if run.done {
        return None;
    }
    while let Ok(Some(line)) = run.lines.next_line().await {
        let now = time::OffsetDateTime::now_utc();
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            run.last_line = now;
            continue;
        };
        run.saw_result |= v["type"] == "result";
        Context::read(&mut run.context, &v);
        run.track_call(&v, now).await;
        run.last_line = now;
        // Only the turn goes back: the init line and hook output describe the host.
        if matches!(
            v["type"].as_str(),
            Some("assistant" | "user" | "stream_event" | "result")
        ) {
            return Some((Ok(Bytes::from(line + "\n")), run));
        }
    }
    run.done = true;
    run.flush_call().await;
    let context = run.context.take();
    if let Some(context) = &context
        && let Ok(raw) = serde_json::to_vec(context)
    {
        let _ = tokio::fs::write(&run.context_file, raw).await;
    }
    let status = run.child.wait().await;
    let stderr = (&mut run.stderr).await.unwrap_or_default();
    if status.as_ref().is_ok_and(std::process::ExitStatus::success) {
        if let Some((file, id)) = run.remember.take() {
            let _ = tokio::fs::write(file, id.to_string()).await;
        }
        // Off the turn's path: the runner's turn ends now, the breakdown comes a moment later.
        if let (Some(context), Some(config)) = (context, run.config.take()) {
            tokio::spawn(measure(
                run.state.clone(),
                run.dir.clone(),
                config,
                std::mem::take(&mut run.measure_args),
                context.at,
            ));
        }
        return None;
    }
    if run.saw_result {
        return None;
    }
    let tail: String = stderr
        .trim()
        .chars()
        .rev()
        .take(400)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let line = json!({ "type": "result", "subtype": "error_during_execution", "is_error": true,
        "result": format!("claude stopped ({}): {tail}", status.map_or_else(|e| e.to_string(), |s| s.to_string())) });
    Some((Ok(Bytes::from(line.to_string() + "\n")), run))
}

// The agent's computer, for `claude` on the host: these MCP tools act inside the agent's sandbox.

const COMPUTER: [(&str, ToolGroup, &str); 3] = [
    (
        "bash",
        ToolGroup::Shell,
        "Run a bash command on your own computer, in your workspace. Times out after 120 s.",
    ),
    (
        "read_file",
        ToolGroup::Files,
        "Read a text file on your own computer. Relative paths are inside your workspace.",
    ),
    (
        "write_file",
        ToolGroup::Files,
        "Write a text file on your own computer, creating folders as needed. Relative paths are inside your workspace.",
    ),
];

fn schema(name: &str) -> Arc<JsonObject> {
    let props = match name {
        "bash" => json!({ "command": { "type": "string" } }),
        "read_file" => json!({ "path": { "type": "string" } }),
        _ => json!({ "path": { "type": "string" }, "content": { "type": "string" } }),
    };
    let required: Vec<&String> = props
        .as_object()
        .map(|p| p.keys().collect())
        .unwrap_or_default();
    let schema = json!({ "type": "object", "properties": props, "required": required });
    Arc::new(schema.as_object().cloned().unwrap_or_default())
}

/// The computer tools an agent gets: none unless it runs on Claude Code, then those of its
/// tool groups.
fn computer_names(kind: Option<ProviderKind>, groups: &[ToolGroup]) -> Vec<&'static str> {
    if kind != Some(ProviderKind::ClaudeCode) {
        return Vec::new();
    }
    COMPUTER
        .iter()
        .filter(|(_, group, _)| groups.contains(group))
        .map(|(name, _, _)| *name)
        .collect()
}

async fn agent_and_kind(state: &AppState, agent: AgentId) -> Option<(Agent, Option<ProviderKind>)> {
    let agent = state.store.get_agent(agent).await.ok()?;
    let kind = crate::agents::provider_of(state, &agent)
        .await
        .map(|p| p.kind);
    Some((agent, kind))
}

pub async fn computer_tools(state: &AppState, agent: AgentId) -> Vec<Tool> {
    let Some((agent, kind)) = agent_and_kind(state, agent).await else {
        return Vec::new();
    };
    let names = computer_names(kind, &agent.tools);
    COMPUTER
        .iter()
        .filter(|(name, _, _)| names.contains(name))
        .map(|(name, _, about)| Tool::new(*name, *about, schema(name)))
        .collect()
}

/// Runs a computer tool, or `None` when `name` is not one (a connector tool then).
pub async fn call_computer(
    state: &AppState,
    agent: AgentId,
    name: &str,
    args: Option<&JsonObject>,
) -> Option<Result<CallToolResult, String>> {
    if !COMPUTER.iter().any(|(n, _, _)| *n == name) {
        return None;
    }
    let Some((agent, kind)) = agent_and_kind(state, agent).await else {
        return Some(Err("unknown agent".into()));
    };
    // A model can call a tool by name without it being listed.
    if !computer_names(kind, &agent.tools).contains(&name) {
        return Some(Err(format!("{name} is turned off for you")));
    }
    let arg = |key: &str| {
        args.and_then(|a| a.get(key))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Some(
        run_computer(
            state,
            &agent,
            name,
            &arg("command"),
            &arg("path"),
            &arg("content"),
        )
        .await,
    )
}

async fn run_computer(
    state: &AppState,
    agent: &Agent,
    name: &str,
    command: &str,
    path: &str,
    content: &str,
) -> Result<CallToolResult, String> {
    let handle = crate::lifecycle::handle_of(agent).ok_or("your computer is not running")?;
    let backend = &state.backend;
    let home = backend.home().trim_end_matches('/').to_owned();
    let workspace = format!("{home}/workspace");
    let resolve = |p: &str| {
        let full = if p.starts_with('/') {
            p.to_owned()
        } else {
            format!("{workspace}/{p}")
        };
        // Files go through the engine as root, so nothing outside the agent's own home.
        if !full.starts_with(&format!("{home}/")) {
            return Err(format!(
                "only files in your home folder ({home}) can be read or written"
            ));
        }
        SandboxPath::parse(&full).map_err(|e| e.to_string())
    };
    let text = match name {
        "bash" => {
            let call = crate::tool_check::Call::Shell(command);
            if let Err(why) = crate::tool_check::gate(state, agent.id, call).await {
                return Ok(CallToolResult::error(vec![ContentBlock::text(why)]));
            }
            let mut req = ExecRequest::new(["timeout", BASH_TIMEOUT_S, "bash", "-lc", command]);
            req.workdir = Some(workspace.clone());
            let out = backend
                .exec(&handle, &req)
                .await
                .map_err(|e| e.to_string())?;
            let status = if out.exit_code == 124 {
                format!("timed out after {BASH_TIMEOUT_S} s")
            } else {
                format!("exit code {}", out.exit_code)
            };
            let stderr = if out.stderr.is_empty() {
                String::new()
            } else {
                format!("\n[stderr]\n{}", out.stderr)
            };
            let text = format!("[{status}]\n{}{stderr}", out.stdout);
            return Ok(if out.exit_code == 0 {
                CallToolResult::success(vec![ContentBlock::text(text)])
            } else {
                CallToolResult::error(vec![ContentBlock::text(text)])
            });
        }
        "read_file" => {
            let bytes = backend
                .read_file(&handle, &resolve(path)?)
                .await
                .map_err(|e| e.to_string())?;
            String::from_utf8_lossy(&bytes)
                .chars()
                .take(MAX_READ_CHARS)
                .collect()
        }
        _ => {
            let file = resolve(path)?;
            if let Some(parent) = Path::new(file.as_str()).parent() {
                let mkdir = ExecRequest::new(["mkdir", "-p", &parent.to_string_lossy()]);
                backend
                    .exec(&handle, &mkdir)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            backend
                .write_file(&handle, &file, content.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            format!("wrote {} bytes to {}", content.len(), file.as_str())
        }
    };
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_turn_for_a_busy_agent_is_refused_without_locking_up() {
        let agent = AgentId::new();
        let first = Busy::take(agent).expect("free");
        assert!(Busy::take(agent).is_none(), "already busy");
        drop(first);
        assert!(Busy::take(agent).is_some(), "free again");
    }

    #[test]
    fn context_is_the_newest_calls_tokens_with_the_window_claude_names() {
        let mut context = None;
        let line = |l: &str| serde_json::from_str::<Value>(l).unwrap();
        Context::read(&mut context, &line(r#"{"type":"system","subtype":"init"}"#));
        assert_eq!(context, None);
        Context::read(
            &mut context,
            &line(
                r#"{"type":"assistant","message":{"model":"m1","usage":{"input_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":5,"output_tokens":7}}}"#,
            ),
        );
        Context::read(
            &mut context,
            &line(
                r#"{"type":"assistant","message":{"model":"m1","usage":{"input_tokens":12,"cache_read_input_tokens":200,"output_tokens":3}}}"#,
            ),
        );
        Context::read(
            &mut context,
            &line(r#"{"type":"result","modelUsage":{"m1":{"contextWindow":200000}}}"#),
        );
        let c = context.unwrap();
        assert_eq!(
            (c.tokens, c.model.as_deref(), c.window),
            (212, Some("m1"), Some(200_000))
        );
    }

    #[test]
    fn context_is_measured_on_the_turns_setup_without_touching_the_session() {
        let id = Uuid::new_v4();
        let files = Path::new("/tmp/x");
        let extensions = Extensions {
            plugins: vec![PathBuf::from("/plugins/fmt")],
            subagents: true,
        };
        let args = context_args(files, id, Some("sonnet"), &extensions);
        let file = |name: &str| files.join(name).display().to_string();
        for want in [
            "-p".to_owned(),
            "--tools=Task".into(),
            "--strict-mcp-config".into(),
            "--setting-sources=".into(),
            "--model=sonnet".into(),
            format!("--resume={id}"),
            format!("--mcp-config={}", file("mcp.json")),
            format!("--append-system-prompt-file={}", file("system.md")),
            format!("--settings={}", file("measure.json")),
            "--plugin-dir=/plugins/fmt".into(),
            "--no-session-persistence".into(),
            "--output-format=json".into(),
        ] {
            assert!(args.contains(&want), "{want} missing: {args:?}");
        }
        assert_eq!(args.last().map(String::as_str), Some("/context"));
        assert!(
            !args.iter().any(|a| a == "--verbose"
                || a.contains("stream-json")
                || *a == format!("--settings={}", file("settings.json"))),
            "no stream, no hooks: {args:?}"
        );
        let dir = write_turn_files("http://127.0.0.1:7879", "t", "s", &settings_file(&[])).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("measure.json")).unwrap(),
            r#"{"disableAllHooks":true}"#
        );
    }

    #[test]
    fn measuring_on_request_reaches_no_mcp_server_and_runs_no_hook() {
        let files = on_request_files("you are scout").unwrap();
        let read = |name: &str| std::fs::read_to_string(files.path().join(name)).unwrap();
        assert_eq!(read("mcp.json"), r#"{"mcpServers":{}}"#);
        assert_eq!(read("system.md"), "you are scout");
        assert_eq!(read("measure.json"), r#"{"disableAllHooks":true}"#);
        assert!(!files.path().join("settings.json").exists());
    }

    #[test]
    fn the_cli_runs_without_built_in_tools_or_the_users_settings() {
        let id = Uuid::nil();
        let extensions = Extensions {
            plugins: vec![PathBuf::from("/plugins/fmt")],
            subagents: false,
        };
        let args = turn_args(
            Path::new("/tmp/x"),
            &Session::Resume(id),
            Some("sonnet"),
            &extensions,
        );
        for want in [
            "-p",
            "--tools=",
            "--strict-mcp-config",
            "--setting-sources=",
            "--permission-mode=dontAsk",
            "--allowedTools=mcp__ferrobot",
            "--model=sonnet",
            "--mcp-config=/tmp/x/mcp.json",
            "--append-system-prompt-file=/tmp/x/system.md",
            "--settings=/tmp/x/settings.json",
            "--plugin-dir=/plugins/fmt",
        ] {
            assert!(args.iter().any(|a| a == want), "{want} missing in {args:?}");
        }
        assert!(args.contains(&format!("--resume={id}")));
        let subagents = Extensions {
            subagents: true,
            ..Extensions::default()
        };
        let with = turn_args(Path::new("/tmp/x"), &Session::Resume(id), None, &subagents);
        let tools: Vec<&String> = with
            .iter()
            .filter(|a| a.starts_with("--tools") || a.starts_with("--allowedTools"))
            .collect();
        assert_eq!(tools, ["--tools=Task", "--allowedTools=mcp__ferrobot,Task"]);
        // `--bare` would stop claude from reading the user's own login.
        assert!(
            !args
                .iter()
                .any(|a| a == "--bare" || a.starts_with("--dangerously"))
        );
        assert!(
            args.iter().all(|a| a.starts_with('-')),
            "values only as --flag=value: {args:?}"
        );
    }

    #[test]
    fn no_key_token_or_parent_claude_reaches_the_cli() {
        for key in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDECODE",
            "FERROBOT_API_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
        ] {
            assert!(foreign(OsStr::new(key)), "{key}");
        }
        for key in ["PATH", "HOME", "CLAUDE_CONFIG_DIR", "USERPROFILE"] {
            assert!(
                !foreign(OsStr::new(key)),
                "{key} is needed to find the user's own login"
            );
        }
    }

    #[test]
    fn plan_limits_keep_only_the_session_and_all_models_week() {
        let report = "You are currently using your subscription to power your Claude Code usage

Current session: 23% used · resets Nov 23 at 9:39pm (Europe/Berlin)
Current week (all models): 64% used · resets Nov 26 at 6:59am (Europe/Berlin)
Current week (Fable): 3% used · resets Nov 26 at 6:59am (Europe/Berlin)

What's contributing to your usage?
Approximate · sessions on this machine only · shares overlap

Last 24h · 1485 requests · 11 sessions
  86%  Review sessions active over many hours · whole sessions with requests in 8 or more separate hours
  Top subagents: general-purpose 12%";
        let window = |used_percent: f64, resets: &str| LimitWindow {
            used_percent,
            resets: Some(resets.to_owned()),
        };
        assert_eq!(
            limits_of(report),
            PlanLimits {
                five_hour: Some(window(23.0, "Nov 23 at 9:39pm (Europe/Berlin)")),
                weekly: Some(window(64.0, "Nov 26 at 6:59am (Europe/Berlin)")),
            }
        );
        assert_eq!(limits_of("Not logged in"), PlanLimits::default());
    }

    #[test]
    fn models_cannot_pose_as_flags() {
        assert!(valid_model("sonnet") && valid_model("claude-opus-5-5") && valid_model("opus[1m]"));
        assert!(!valid_model("--dangerously-skip-permissions"));
        assert!(!valid_model("sonnet --tools=default") && !valid_model(""));
    }

    #[test]
    fn status_keeps_the_login_state_and_drops_the_account() {
        let out =
            r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "someone@example.com"}"#;
        assert_eq!(login_of(out), (true, Some("claude.ai".into())));
        assert_eq!(login_of("not json"), (false, None));
    }

    #[test]
    fn computer_tools_are_only_for_claude_code_agents_and_their_groups() {
        let all = ToolGroup::ALL;
        assert_eq!(computer_names(Some(ProviderKind::Anthropic), &all), [""; 0]);
        assert_eq!(computer_names(None, &all), [""; 0]);
        assert_eq!(
            computer_names(Some(ProviderKind::ClaudeCode), &all),
            ["bash", "read_file", "write_file"]
        );
        assert_eq!(
            computer_names(Some(ProviderKind::ClaudeCode), &[ToolGroup::Files]),
            ["read_file", "write_file"]
        );
    }

    #[test]
    fn host_file_references_and_commands_are_defused() {
        let defused =
            defuse("read @/etc/hosts, @../../x, @~/.ssh/id, @C:\\x and @\"/a b\"").unwrap();
        assert_eq!(
            defused,
            "read @ /etc/hosts, @ ../../x, @ ~/.ssh/id, @ C:\\x and @ \"/a b\""
        );
        let plain = "Message #3 from @bob: mail tim@example.com, ping @ada";
        assert_eq!(
            defuse(plain).unwrap(),
            plain,
            "mentions and emails stay as they are"
        );
        assert!(defuse("/cost").is_err() && defuse("  !touch x").is_err());
        assert!(defuse("\u{feff}/cost").is_err() && defuse("\u{200b} !x").is_err());
        let hidden = defuse("see @x\u{85}/../../etc/hosts").unwrap();
        assert!(
            hidden.contains("@ x"),
            "a path hidden behind U+0085 is defused: {hidden}"
        );
    }

    #[test]
    fn the_turn_files_are_private_and_name_only_the_daemon() {
        let dir = write_turn_files(
            "http://127.0.0.1:7879/",
            "agent-token",
            "be brief",
            &settings_file(&[]),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("settings.json")).unwrap(),
            r#"{"hooks":{}}"#
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("system.md")).unwrap(),
            "be brief"
        );
        let path = dir.path().join("mcp.json");
        let config: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let servers = config["mcpServers"].as_object().unwrap();
        assert_eq!(servers.keys().collect::<Vec<_>>(), ["ferrobot"]);
        assert_eq!(servers["ferrobot"]["url"], "http://127.0.0.1:7879/mcp");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    fn hook(event: HookEvent, matcher: Option<&str>, command: &str) -> Hook {
        Hook {
            event,
            matcher: matcher.map(str::to_owned),
            command: command.into(),
        }
    }

    #[test]
    fn the_settings_file_holds_only_the_operators_hooks() {
        let file = settings_file(&[
            hook(
                HookEvent::PreToolUse,
                Some("mcp__ferrobot__bash"),
                "guard.sh",
            ),
            hook(HookEvent::PreToolUse, None, "log.sh"),
            hook(HookEvent::Stop, None, "notify.sh"),
        ]);
        assert_eq!(
            file,
            json!({ "hooks": {
                "PreToolUse": [
                    { "matcher": "mcp__ferrobot__bash", "hooks": [{ "type": "command", "command": "guard.sh" }] },
                    { "hooks": [{ "type": "command", "command": "log.sh" }] },
                ],
                "Stop": [{ "hooks": [{ "type": "command", "command": "notify.sh" }] }],
            }})
        );
    }

    #[test]
    fn hooks_and_plugins_are_checked() {
        let mut ok = ClaudeCodeSettings {
            hooks: vec![hook(HookEvent::PostToolUse, Some("  "), "  fmt.sh ")],
            plugins: vec!["fmt@market".into(), "fmt@market".into()],
            subagents: false,
        };
        validate(&mut ok).unwrap();
        assert_eq!(ok.hooks[0], hook(HookEvent::PostToolUse, None, "fmt.sh"));
        assert_eq!(ok.plugins, ["fmt@market"]);
        for bad in [
            ClaudeCodeSettings {
                hooks: vec![hook(HookEvent::Stop, Some("Bash"), "x")],
                ..ClaudeCodeSettings::default()
            },
            ClaudeCodeSettings {
                hooks: vec![hook(HookEvent::Stop, None, " ")],
                ..ClaudeCodeSettings::default()
            },
            ClaudeCodeSettings {
                plugins: vec!["--plugin-dir=/x".into()],
                ..ClaudeCodeSettings::default()
            },
            ClaudeCodeSettings {
                plugins: vec!["fmt@../x".into()],
                ..ClaudeCodeSettings::default()
            },
        ] {
            assert!(validate(&mut bad.clone()).is_err(), "{bad:?}");
        }
    }
}
