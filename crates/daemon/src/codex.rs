//! Host-side `codex app-server` for `ChatGPT` sign-in, so its tokens never enter a sandbox.
//! Its `auth.json` lives in the secret store and is copied into a private `CODEX_HOME` per run.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use uuid::Uuid;

use crate::AppState;

/// Where the `ChatGPT` Codex backend lives (the same URL Codex uses itself).
pub const CHATGPT_CODEX_UPSTREAM: &str = "https://chatgpt.com/backend-api/codex";
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Sign-in progress per provider (in memory; a restart forgets pending ones).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LoginStatus {
    /// Not signed in.
    None,
    /// Waiting for the operator to confirm the code in the browser.
    Pending,
    SignedIn,
    Failed {
        error: String,
    },
}

#[derive(Default)]
pub struct CodexState {
    logins: HashMap<Uuid, LoginStatus>,
    /// Access token, `ChatGPT` account id, and when to stop using them.
    tokens: HashMap<Uuid, (String, String, Instant)>,
}

impl std::fmt::Debug for CodexState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexState")
            .field("logins", &self.logins)
            .field("cached_tokens", &self.tokens.len())
            .finish()
    }
}

/// A device code the operator enters at `verification_url`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct DeviceCode {
    pub verification_url: String,
    pub user_code: String,
}

/// `bin` on PATH or where Codex's installers and npm put it: an app started from the Dock or the
/// Start menu has a shorter PATH than a terminal, and on Windows npm installs a `.cmd`.
pub(crate) fn find_bin(bin: &str, path: Option<&OsStr>, extra: &[PathBuf]) -> PathBuf {
    if Path::new(bin).components().count() > 1 {
        return bin.into();
    }
    let names = if cfg!(windows) && Path::new(bin).extension().is_none() {
        vec![format!("{bin}.exe"), format!("{bin}.cmd")]
    } else {
        vec![bin.to_owned()]
    };
    path.map(std::env::split_paths)
        .into_iter()
        .flatten()
        .chain(extra.iter().cloned())
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| bin.into())
}

/// Where the Codex and Claude Code installers and npm put their CLIs.
pub(crate) fn install_dirs() -> Vec<PathBuf> {
    let env = |key| std::env::var_os(key).map(PathBuf::from);
    if cfg!(windows) {
        vec![
            env("LOCALAPPDATA").map(|d| d.join(r"Programs\OpenAI\Codex\bin")),
            env("APPDATA").map(|d| d.join("npm")),
            env("USERPROFILE").map(|d| d.join(r".local\bin")),
        ]
    } else {
        vec![
            env("HOME").map(|h| h.join(".local/bin")),
            Some("/opt/homebrew/bin".into()),
            Some("/usr/local/bin".into()),
        ]
    }
    .into_iter()
    .flatten()
    .collect()
}

/// A running `codex app-server`, spoken to over stdio (one JSON object per
/// line, no `jsonrpc` field).
struct AppServer {
    _child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    notifications: Vec<(String, Value)>,
}

impl AppServer {
    async fn start(bin: &str, home: &Path) -> Result<Self, String> {
        tokio::fs::create_dir_all(home)
            .await
            .map_err(|e| format!("codex home: {e}"))?;
        let path = std::env::var_os("PATH");
        let mut child = Command::new(find_bin(bin, path.as_deref(), &install_dirs()))
            .arg("app-server")
            .env("CODEX_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    tracing::warn!(bin, "codex not found; --codex-bin sets its path");
                    "Codex is not installed on the daemon's computer".to_owned()
                }
                _ => format!("cannot start `{bin} app-server` on the daemon's computer: {e}"),
            })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err("codex app-server has no stdio".into());
        };
        let mut server = Self {
            _child: child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 0,
            notifications: Vec::new(),
        };
        let info = json!({"clientInfo": {"name": "ferrobot", "title": "ferrobot",
                                         "version": env!("CARGO_PKG_VERSION")}});
        server.request("initialize", info).await?;
        server.send(&json!({"method": "initialized"})).await?;
        Ok(server)
    }

    async fn send(&mut self, message: &Value) -> Result<(), String> {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("codex app-server: {e}"))
    }

    async fn read(&mut self) -> Result<Value, String> {
        match self.lines.next_line().await {
            Ok(Some(line)) => serde_json::from_str(&line).map_err(|e| format!("codex: {e}")),
            Ok(None) => Err("codex app-server exited".into()),
            Err(e) => Err(format!("codex app-server: {e}")),
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"id": id, "method": method, "params": params}))
            .await?;
        tokio::time::timeout(RPC_TIMEOUT, async {
            loop {
                let msg = self.read().await?;
                match (msg.get("id"), msg.get("method")) {
                    (Some(got), None) if got == id => {
                        return match msg.get("error") {
                            Some(e) => Err(format!("codex {method}: {e}")),
                            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                        };
                    }
                    // A request from the server: we answer none of them here.
                    (Some(other), Some(_)) => {
                        let refuse = json!({"id": other, "error": {"code": -32601, "message": "not supported"}});
                        self.send(&refuse).await?;
                    }
                    (None, Some(m)) => {
                        let m = m.as_str().unwrap_or_default().to_owned();
                        self.notifications
                            .push((m, msg.get("params").cloned().unwrap_or(Value::Null)));
                    }
                    _ => {}
                }
            }
        })
        .await
        .map_err(|_| format!("codex {method}: no answer in time"))?
    }

    async fn wait_for(&mut self, method: &str) -> Result<Value, String> {
        loop {
            if let Some(i) = self.notifications.iter().position(|(m, _)| m == method) {
                return Ok(self.notifications.remove(i).1);
            }
            let msg = self.read().await?;
            if let (None, Some(m)) = (msg.get("id"), msg.get("method")) {
                let m = m.as_str().unwrap_or_default().to_owned();
                self.notifications
                    .push((m, msg.get("params").cloned().unwrap_or(Value::Null)));
            }
        }
    }
}

/// Where a provider's Codex sign-in (Codex's `auth.json`) is kept.
pub fn secret_key(provider: Uuid) -> String {
    format!("codex-auth:{provider}")
}

/// Private `CODEX_HOME`s of running app-servers, under the codex dir.
fn run_dir(codex_home: &Path) -> std::path::PathBuf {
    codex_home.join("run")
}

/// The codex dir, without private homes a crash left behind (they hold
/// copies of `ChatGPT` sign-ins).
pub fn clean_home(codex_home: std::path::PathBuf) -> std::path::PathBuf {
    let _ = std::fs::remove_dir_all(run_dir(&codex_home));
    codex_home
}

/// A `CODEX_HOME` that exists while one app-server runs (0700, `auth.json`
/// 0600); removed on drop.
struct Home {
    dir: tempfile::TempDir,
    provider: Uuid,
    stored: Option<String>,
}

fn io(e: impl std::fmt::Display) -> String {
    format!("codex home: {e}")
}

impl Home {
    async fn open(state: &AppState, provider: Uuid) -> Result<Self, String> {
        let root = run_dir(&state.settings.codex_home);
        tokio::fs::create_dir_all(&root).await.map_err(io)?;
        let dir = tempfile::Builder::new()
            .prefix("home-")
            .tempdir_in(&root)
            .map_err(io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
                .map_err(io)?;
        }
        let secrets = &state.settings.secrets;
        let key = secret_key(provider);
        let mut stored = secrets.get(&key).await.map_err(|e| e.to_string())?;
        // Sign-ins from before the secret store: move them over once.
        let legacy = state.settings.codex_home.join(provider.to_string());
        if stored.is_none()
            && let Ok(auth) = tokio::fs::read_to_string(legacy.join("auth.json")).await
        {
            secrets.set(&key, &auth).await.map_err(|e| e.to_string())?;
            stored = Some(auth);
        }
        let _ = tokio::fs::remove_dir_all(&legacy).await;
        if let Some(auth) = &stored {
            let mut file = std::fs::OpenOptions::new();
            file.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut file, 0o600);
            std::io::Write::write_all(
                &mut file.open(dir.path().join("auth.json")).map_err(io)?,
                auth.as_bytes(),
            )
            .map_err(io)?;
        }
        Ok(Self {
            dir,
            provider,
            stored,
        })
    }

    /// Keeps what Codex wrote, unless another run replaced the stored
    /// sign-in meanwhile (a new sign-in must not lose to a stale refresh).
    async fn save(&self, state: &AppState) -> Result<(), String> {
        let Ok(auth) = tokio::fs::read_to_string(self.dir.path().join("auth.json")).await else {
            return Ok(());
        };
        if Some(&auth) == self.stored.as_ref() {
            return Ok(());
        }
        let _codex = state.codex.lock().await;
        let (secrets, key) = (&state.settings.secrets, secret_key(self.provider));
        if secrets.get(&key).await.map_err(|e| e.to_string())? != self.stored {
            return Ok(());
        }
        secrets.set(&key, &auth).await.map_err(|e| e.to_string())
    }
}

/// Starts a `ChatGPT` device-code sign-in on the host; it completes in the background.
pub async fn start_login(state: &AppState, provider: Uuid) -> Result<DeviceCode, String> {
    let home = Home::open(state, provider).await?;
    let mut server = AppServer::start(&state.settings.codex_bin, home.dir.path()).await?;
    let started = server
        .request("account/login/start", json!({"type": "chatgptDeviceCode"}))
        .await?;
    let field = |k: &str| started[k].as_str().map(str::to_owned);
    let (Some(verification_url), Some(user_code)) = (field("verificationUrl"), field("userCode"))
    else {
        return Err(format!("unexpected login answer: {started}"));
    };
    state
        .codex
        .lock()
        .await
        .logins
        .insert(provider, LoginStatus::Pending);
    let state = state.clone();
    tokio::spawn(async move {
        let done = tokio::time::timeout(LOGIN_TIMEOUT, server.wait_for("account/login/completed"))
            .await
            .unwrap_or_else(|_| Err("not confirmed in time".into()));
        let saved = home.save(&state).await;
        drop(server);
        let status = match done.and_then(|p| saved.map(|()| p)) {
            Ok(p) if p["success"] == true => LoginStatus::SignedIn,
            Ok(p) => LoginStatus::Failed {
                error: p["error"].as_str().unwrap_or("sign-in failed").to_owned(),
            },
            Err(error) => LoginStatus::Failed { error },
        };
        let signed_in = status == LoginStatus::SignedIn;
        {
            let mut codex = state.codex.lock().await;
            codex.tokens.remove(&provider);
            codex.logins.insert(provider, status);
        }
        if signed_in {
            crate::credentials::clear_provider(&state, provider).await;
        }
    });
    Ok(DeviceCode {
        verification_url,
        user_code,
    })
}

pub async fn login_status(state: &AppState, provider: Uuid) -> LoginStatus {
    if let Some(s) = state.codex.lock().await.logins.get(&provider)
        && *s != LoginStatus::SignedIn
    {
        return s.clone();
    }
    match auth_status(state, provider, false).await {
        Ok(status) if status["authMethod"] == "chatgpt" => LoginStatus::SignedIn,
        Ok(_) => LoginStatus::None,
        Err(error) => LoginStatus::Failed { error },
    }
}

async fn auth_status(state: &AppState, provider: Uuid, token: bool) -> Result<Value, String> {
    let home = Home::open(state, provider).await?;
    let mut server = AppServer::start(&state.settings.codex_bin, home.dir.path()).await?;
    let status = server
        .request(
            "getAuthStatus",
            json!({"includeToken": token, "refreshToken": token}),
        )
        .await?;
    // A token refresh may have rewritten auth.json.
    home.save(state).await?;
    Ok(status)
}

/// Forgets a provider's sign-in (the provider was deleted).
pub async fn forget(state: &AppState, provider: Uuid) -> Result<(), String> {
    let _ = tokio::fs::remove_dir_all(state.settings.codex_home.join(provider.to_string())).await;
    let mut codex = state.codex.lock().await;
    codex.tokens.remove(&provider);
    codex.logins.remove(&provider);
    state
        .settings
        .secrets
        .delete(&secret_key(provider))
        .await
        .map_err(|e| e.to_string())
}

/// A fresh `ChatGPT` access token and account id for the proxy.
pub async fn chatgpt_token(state: &AppState, provider: Uuid) -> Result<(String, String), String> {
    if let Some((token, account, until)) = state.codex.lock().await.tokens.get(&provider)
        && Instant::now() < *until
    {
        return Ok((token.clone(), account.clone()));
    }
    let status = auth_status(state, provider, true).await?;
    let token = status["authToken"]
        .as_str()
        .filter(|_| status["authMethod"] == "chatgpt")
        .ok_or("this provider is not signed in with ChatGPT; sign in or set an API key")?
        .to_owned();
    let claims = jwt_claims(&token).ok_or("ChatGPT token is not a JWT")?;
    let account = claims["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .ok_or("ChatGPT token has no account id")?
        .to_owned();
    let lifetime = claims["exp"]
        .as_u64()
        .and_then(|exp| exp.checked_sub(unix_now()))
        .map_or(Duration::ZERO, Duration::from_secs);
    // Ask Codex again well before expiry; it refreshes the token itself.
    let keep = lifetime
        .saturating_sub(Duration::from_secs(300))
        .min(Duration::from_secs(1800));
    state.codex.lock().await.tokens.insert(
        provider,
        (token.clone(), account.clone(), Instant::now() + keep),
    );
    Ok((token, account))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The (unverified) claims of a JWT; only used to read our own token's metadata.
fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_is_found_outside_path() {
        let (on_path, installed) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let name = if cfg!(windows) { "codex.cmd" } else { "codex" };
        std::fs::write(installed.path().join(name), "").unwrap();
        let path = std::env::join_paths([on_path.path()]).unwrap();
        let extra = [installed.path().to_path_buf()];
        assert_eq!(
            find_bin("codex", Some(&path), &extra),
            installed.path().join(name)
        );
        assert_eq!(find_bin("codex", Some(&path), &[]), PathBuf::from("codex"));
        assert_eq!(
            find_bin("/opt/codex", None, &extra),
            PathBuf::from("/opt/codex")
        );
    }

    #[test]
    fn debug_never_shows_tokens() {
        let mut state = CodexState::default();
        let id = Uuid::nil();
        state
            .tokens
            .insert(id, ("eyJsecret".into(), "acct".into(), Instant::now()));
        state.logins.insert(id, LoginStatus::SignedIn);
        let shown = format!("{state:?}");
        assert!(
            !shown.contains("eyJsecret") && shown.contains("cached_tokens: 1"),
            "{shown}"
        );
        let gcloud = crate::proxy::CachedToken(Some(("ya29.secret".into(), Instant::now())));
        assert!(!format!("{gcloud:?}").contains("ya29"));
    }

    #[test]
    fn reads_account_and_expiry_claims() {
        let token = "eyJhbGciOiJub25lIn0.eyJleHAiOjQxMDI0NDQ4MDAsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2N0LTEyMyJ9fQ.sig";
        let claims = jwt_claims(token).unwrap();
        assert_eq!(claims["exp"], 4_102_444_800_u64);
        assert_eq!(
            claims["https://api.openai.com/auth"]["chatgpt_account_id"],
            "acct-123"
        );
        assert!(jwt_claims("not-a-jwt").is_none());
    }
}
