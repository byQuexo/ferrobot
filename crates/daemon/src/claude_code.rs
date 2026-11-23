//! The opt-in Claude Code driver: the daemon host's own, unmodified `claude` CLI, signed in with
//! the user's own Claude account through Anthropic's flow. ferrobot never reads, stores or relays
//! that login; `claude` runs on the host with its built-in tools off and reaches the agent's
//! sandbox only through the MCP tools below.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
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

/// Variables the daemon may hold that would move `claude` off the user's own login (keys,
/// tokens, another provider) or make it think it runs inside another Claude Code.
fn foreign(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    key.starts_with("ANTHROPIC_") || key.starts_with("CLAUDE_CODE_") || key == "CLAUDECODE"
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

/// Why a session could not be compacted.
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
    if state.hub.is_working(agent) {
        return Err(CompactError::NotNow(
            "the agent is in a turn; try again when it is done",
        ));
    }
    let _busy = Busy::take(agent).ok_or(CompactError::NotNow(
        "the agent is in a turn; try again when it is done",
    ))?;
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
    Ok(report["result"].as_str().unwrap_or_default().to_owned())
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
fn turn_args(files: &Path, session: &Session, model: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format=stream-json",
        "--verbose",
        "--include-partial-messages",
        // No built-in tool may run on the host; the sandbox is reached through MCP only.
        "--tools=",
        "--strict-mcp-config",
        "--allowedTools=mcp__ferrobot",
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
    args.push(match session {
        Session::New(id) => format!("--session-id={id}"),
        Session::Resume(id) => format!("--resume={id}"),
    });
    if let Some(model) = model {
        args.push(format!("--model={model}"));
    }
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
        let mut busy = BUSY.lock().unwrap_or_else(PoisonError::into_inner);
        busy.insert(agent).then_some(Self(agent))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.0);
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
    let raw = tokio::fs::read(agent_dir(state, agent).join("context.json"))
        .await
        .ok()?;
    serde_json::from_slice(&raw).ok()
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
    _config: tempfile::TempDir,
    _busy: Busy,
    /// Where a new session's id is kept once its first turn succeeded.
    remember: Option<(PathBuf, Uuid)>,
    saw_result: bool,
    done: bool,
    context: Option<Context>,
    context_file: PathBuf,
    /// For the trace: the agent's turn, when the last line came and the call streaming now.
    state: AppState,
    agent: AgentId,
    turn: Option<String>,
    last_line: time::OffsetDateTime,
    call: Option<ModelCall>,
}

impl Run {
    /// Follows the model calls in `claude`'s stream; each becomes a `model_call` event.
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
        if let Some(id) = id {
            self.call = Some(ModelCall {
                id: id.to_owned(),
                model: line["message"]["model"].as_str().map(str::to_owned),
                started: self.last_line,
                ended: now,
                usage: line["message"]["usage"].clone(),
            });
        }
    }

    async fn flush_call(&mut self) {
        let Some(c) = self.call.take() else {
            return;
        };
        let rfc = |t: time::OffsetDateTime| {
            t.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
        };
        let u = &c.usage;
        let payload = json!({
            "model": c.model,
            "started_at": rfc(c.started),
            "ended_at": rfc(c.ended),
            "input_tokens": u["input_tokens"],
            "output_tokens": u["output_tokens"],
            "cache_read_tokens": u["cache_read_input_tokens"],
            "cache_write_tokens": u["cache_creation_input_tokens"],
        });
        self.state
            .record(self.agent, self.turn.clone(), "model_call", payload)
            .await;
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
    let provider = match agent.provider_id {
        Some(id) => state.store.get_provider(id).await.ok(),
        None => None,
    };
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
    let model = agent.model.clone().or(provider.default_model);
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
    let remember = match session {
        Session::New(id) => Some((session_file, id)),
        Session::Resume(_) => None,
    };
    let config = write_turn_files(&state.settings.agent_url, token, &req.system)
        .map_err(|e| e.to_string())?;
    let mut cmd = claude(state);
    cmd.args(turn_args(config.path(), &session, model))
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
        _config: config,
        _busy: busy,
        remember,
        saw_result: false,
        done: false,
        context: None,
        context_file,
        state: state.clone(),
        agent: agent.id,
        turn: state.hub.current_turn(agent.id),
        last_line: time::OffsetDateTime::now_utc(),
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

/// The agent's own token for the daemon's MCP server and its system prompt, in private files
/// rather than argv.
fn write_turn_files(
    agent_url: &str,
    token: &str,
    system: &str,
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

/// `claude` acts on prompt text before the model sees it: it attaches files named as `@path`
/// (also outside its folder) and runs a leading `/command`. Those come from the sandbox or from
/// other agents' messages, so a path after `@` gets a space and a leading `/` or `!` is refused.
fn defuse(prompt: &str) -> Result<String, ()> {
    if matches!(prompt.trim_start().chars().next(), Some('/' | '!')) {
        return Err(());
    }
    let mut out = String::with_capacity(prompt.len());
    let mut rest = prompt;
    while let Some(at) = rest.find('@') {
        out.push_str(&rest[..=at]);
        rest = &rest[at + 1..];
        let token = rest.split(char::is_whitespace).next().unwrap_or_default();
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
    if let Ok(Some(line)) = run.lines.next_line().await {
        let now = time::OffsetDateTime::now_utc();
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            run.saw_result |= v["type"] == "result";
            Context::read(&mut run.context, &v);
            run.track_call(&v, now).await;
        }
        run.last_line = now;
        return Some((Ok(Bytes::from(line + "\n")), run));
    }
    run.done = true;
    run.flush_call().await;
    if let Some(context) = run.context.take()
        && let Ok(raw) = serde_json::to_vec(&context)
    {
        let _ = tokio::fs::write(&run.context_file, raw).await;
    }
    let status = run.child.wait().await;
    let stderr = (&mut run.stderr).await.unwrap_or_default();
    if status.as_ref().is_ok_and(std::process::ExitStatus::success) {
        if let Some((file, id)) = run.remember.take() {
            let _ = tokio::fs::write(file, id.to_string()).await;
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
    let kind = match agent.provider_id {
        Some(id) => state.store.get_provider(id).await.ok().map(|p| p.kind),
        None => None,
    };
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
    let workspace = format!("{}/workspace", backend.home().trim_end_matches('/'));
    let resolve = |p: &str| {
        let full = if p.starts_with('/') {
            p.to_owned()
        } else {
            format!("{workspace}/{p}")
        };
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
    fn the_cli_runs_without_built_in_tools_or_the_users_settings() {
        let id = Uuid::nil();
        let args = turn_args(Path::new("/tmp/x"), &Session::Resume(id), Some("sonnet"));
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
        ] {
            assert!(args.iter().any(|a| a == want), "{want} missing in {args:?}");
        }
        assert!(args.contains(&format!("--resume={id}")));
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
    }

    #[test]
    fn the_turn_files_are_private_and_name_only_the_daemon() {
        let dir = write_turn_files("http://127.0.0.1:7879/", "agent-token", "be brief").unwrap();
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
}
