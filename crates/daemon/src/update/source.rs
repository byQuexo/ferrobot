//! Source builds pull `main`, rebuild and re-exec themselves. Compiled into debug builds only, so
//! a release binary cannot run it.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::{UpdateInfo, UpdateState};
use crate::AppState;
use crate::error::ApiError;

const LOG_LINES: usize = 30;
const PULL_TIMEOUT: Duration = Duration::from_secs(120);
const BUILD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const NPM: &str = if cfg!(windows) { "npm.cmd" } else { "npm" };

/// The checkout this daemon was built in and what runs the update steps.
#[derive(Debug, Clone)]
pub struct Source {
    /// This daemon's binary, `<repo>/target/debug/ferrobotd`.
    pub exe: PathBuf,
    pub repo: PathBuf,
    /// `HEAD` when the daemon started.
    pub commit: String,
    pub runner: Arc<dyn Runner>,
}

impl Source {
    /// The ferrobot checkout `exe` was built in, if it is `<repo>/target/debug/ferrobotd`.
    pub fn detect(exe: PathBuf, runner: Arc<dyn Runner>) -> Option<Self> {
        let debug = exe.parent()?;
        let target = debug.parent()?;
        if debug.file_name()? != "debug" || target.file_name()? != "target" {
            return None;
        }
        let repo = target.parent()?.to_owned();
        let manifest = std::fs::read_to_string(repo.join("crates/daemon/Cargo.toml")).ok()?;
        if !manifest.contains("name = \"ferrobot-daemon\"") || !repo.join(".git").exists() {
            return None;
        }
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["rev-parse", "HEAD"])
            .stdin(Stdio::null())
            .output()
            .ok()?;
        let commit = String::from_utf8(out.stdout).ok()?.trim().to_owned();
        (out.status.success() && !commit.is_empty()).then_some(Self {
            exe,
            repo,
            commit,
            runner,
        })
    }
}

/// Runs the update steps; tests swap in a fake.
#[async_trait]
pub trait Runner: Send + Sync + std::fmt::Debug {
    /// Runs `cmd` in `dir` to completion, passing each output line to `log`.
    async fn run(
        &self,
        dir: &Path,
        cmd: &[&str],
        log: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(), String>;
    /// Replaces this process with `exe`, same arguments and environment; returns only on failure.
    fn restart(&self, exe: &Path) -> Result<(), String>;
}

/// Runs the real commands.
#[derive(Debug)]
pub struct Commands;

#[async_trait]
impl Runner for Commands {
    async fn run(
        &self,
        dir: &Path,
        cmd: &[&str],
        log: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(), String> {
        let (program, args) = cmd.split_first().ok_or("empty command")?;
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            // npm install scripts run here; the daemon's secrets stay out.
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("FERROBOT_API_TOKEN")
            .env("GIT_TERMINAL_PROMPT", "0");
        // A locked SSH key fails the pull instead of waiting for a passphrase.
        if std::env::var_os("GIT_SSH_COMMAND").is_none() {
            command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start {program}: {e}"))?;
        let mut out = BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
        let mut err = BufReader::new(child.stderr.take().ok_or("no stderr")?).lines();
        let (mut out_open, mut err_open) = (true, true);
        while out_open || err_open {
            tokio::select! {
                line = out.next_line(), if out_open => match line {
                    Ok(Some(line)) => log(line),
                    _ => out_open = false,
                },
                line = err.next_line(), if err_open => match line {
                    Ok(Some(line)) => log(line),
                    _ => err_open = false,
                },
            }
        }
        let status = child.wait().await.map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("`{}` failed ({status})", cmd.join(" ")))
        }
    }

    #[cfg(unix)]
    fn restart(&self, exe: &Path) -> Result<(), String> {
        use std::os::unix::process::CommandExt;
        let e = std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .exec();
        Err(format!("cannot restart {}: {e}", exe.display()))
    }

    #[cfg(not(unix))]
    fn restart(&self, _: &Path) -> Result<(), String> {
        Err("rebuilt; restart ferrobotd and the app yourself".into())
    }
}

fn not_source() -> ApiError {
    ApiError::NotFound(
        "not a source build: run a debug build from a ferrobot checkout (target/debug)".into(),
    )
}

fn set(state: &AppState, change: impl FnOnce(&mut UpdateInfo)) {
    change(&mut state.update.lock().unwrap_or_else(PoisonError::into_inner));
}

pub(crate) fn info(state: &AppState) -> UpdateInfo {
    let mut info = state
        .update
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    info.version = env!("CARGO_PKG_VERSION").into();
    if let Some(source) = &state.settings.source_update {
        info.source_build = true;
        info.commit = Some(source.commit.clone());
    }
    info
}

/// Checks the checkout and starts the update in the background.
pub(crate) async fn start(state: &AppState) -> Result<UpdateInfo, ApiError> {
    let source = state
        .settings
        .source_update
        .clone()
        .ok_or_else(not_source)?;
    preflight(&source.repo).await?;
    {
        let mut status = state.update.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(status.state, UpdateState::Running | UpdateState::Restarting) {
            return Err(ApiError::Conflict("an update is already running".into()));
        }
        *status = UpdateInfo {
            state: UpdateState::Running,
            ..UpdateInfo::default()
        };
    }
    let job = state.clone();
    tokio::spawn(async move { run(&job, &source).await });
    Ok(info(state))
}

/// Refuses a checkout `git pull --ff-only` could break: changed tracked files, or not on `main`.
async fn preflight(repo: &Path) -> Result<(), ApiError> {
    let branch = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .map_err(ApiError::Conflict)?;
    if branch != "main" {
        return Err(ApiError::Conflict(format!(
            "the checkout is on {branch}; switch to main to update"
        )));
    }
    let changes = git(repo, &["status", "--porcelain", "--untracked-files=no"])
        .await
        .map_err(ApiError::Conflict)?;
    if !changes.is_empty() {
        return Err(ApiError::Conflict(
            "the checkout has uncommitted changes; commit or stash them first".into(),
        ));
    }
    Ok(())
}

async fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

async fn run(state: &AppState, source: &Source) {
    match steps(state, source).await {
        Ok(false) => set(state, |s| {
            s.state = UpdateState::UpToDate;
            s.step = None;
        }),
        Ok(true) => {
            set(state, |s| {
                s.state = UpdateState::Restarting;
                s.step = None;
            });
            tracing::info!(exe = %source.exe.display(), "update built; restarting");
            // Only a fake runner returns Ok; the real one replaces this process.
            if let Err(e) = source.runner.restart(&source.exe) {
                set(state, |s| {
                    s.state = UpdateState::Failed;
                    s.step = Some("restart".into());
                    s.error = Some(e);
                });
            }
        }
        Err(e) => set(state, |s| {
            s.state = UpdateState::Failed;
            s.error = Some(e);
        }),
    }
}

/// Pulls and rebuilds; `Ok(false)` when this daemon already runs the latest commit.
async fn steps(state: &AppState, source: &Source) -> Result<bool, String> {
    let repo = source.repo.as_path();
    let desktop = repo.join("apps/desktop");
    step(
        state,
        source,
        repo,
        &["git", "pull", "--ff-only"],
        PULL_TIMEOUT,
    )
    .await?;
    let head = git(repo, &["rev-parse", "HEAD"]).await?;
    if head == source.commit {
        return Ok(false);
    }
    let lock = git(
        repo,
        &[
            "diff",
            "--name-only",
            &source.commit,
            &head,
            "--",
            "apps/desktop/package-lock.json",
        ],
    )
    .await?;
    if !lock.is_empty() || !desktop.join("node_modules").is_dir() {
        step(state, source, &desktop, &[NPM, "ci"], BUILD_TIMEOUT).await?;
    }
    step(
        state,
        source,
        &desktop,
        &[NPM, "run", "build"],
        BUILD_TIMEOUT,
    )
    .await?;
    let daemon = ["cargo", "build", "-p", "ferrobot-daemon"];
    step(state, source, repo, &daemon, BUILD_TIMEOUT).await?;
    let shell = desktop.join("src-tauri");
    step(state, source, &shell, &["cargo", "build"], BUILD_TIMEOUT).await?;
    Ok(true)
}

async fn step(
    state: &AppState,
    source: &Source,
    dir: &Path,
    cmd: &[&str],
    timeout: Duration,
) -> Result<(), String> {
    set(state, |s| s.step = Some(cmd.join(" ")));
    let log = |line: String| {
        set(state, |s| {
            s.log.push(line.chars().take(300).collect());
            let extra = s.log.len().saturating_sub(LOG_LINES);
            s.log.drain(..extra);
        });
    };
    tokio::time::timeout(timeout, source.runner.run(dir, cmd, &log))
        .await
        .map_err(|_| format!("timed out after {} s", timeout.as_secs()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A committed ferrobot-like checkout on `main` with a built daemon.
    fn checkout() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir_all(repo.join("crates/daemon")).unwrap();
        std::fs::create_dir_all(repo.join("target/debug")).unwrap();
        std::fs::write(
            repo.join("crates/daemon/Cargo.toml"),
            "[package]\nname = \"ferrobot-daemon\"\n",
        )
        .unwrap();
        std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(repo.join("target/debug/ferrobotd"), "").unwrap();
        sh(repo, &["init", "-q", "-b", "main"]);
        sh(repo, &["add", "."]);
        sh(repo, &["commit", "-q", "-m", "init"]);
        dir
    }

    /// Why `POST /v1/update` refuses the checkout after `change`, if it does.
    async fn refusal(change: impl FnOnce(&Path)) -> Option<String> {
        let dir = checkout();
        change(dir.path());
        let exe = dir.path().join("target/debug/ferrobotd");
        match Source::detect(exe, Arc::new(Commands)) {
            Some(source) => preflight(&source.repo).await.err().map(|e| e.to_string()),
            None => Some(not_source().to_string()),
        }
    }

    #[tokio::test]
    async fn only_a_clean_main_checkout_updates() {
        assert_eq!(refusal(|_| {}).await, None);
        let dirty = refusal(|r| std::fs::write(r.join(".gitignore"), "x\n").unwrap()).await;
        assert!(dirty.unwrap().contains("uncommitted changes"));
        let branch = refusal(|r| sh(r, &["checkout", "-q", "-b", "topic"])).await;
        assert!(branch.unwrap().contains("switch to main"));
        let manifest = "[package]\nname = \"other\"\n";
        let other =
            refusal(|r| std::fs::write(r.join("crates/daemon/Cargo.toml"), manifest).unwrap());
        assert!(other.await.unwrap().contains("not a source build"));
    }

    #[tokio::test]
    async fn commands_log_both_streams_and_report_failures() {
        let lines = std::sync::Mutex::new(Vec::new());
        let log = |line: String| lines.lock().unwrap().push(line);
        let here = Path::new(".");
        Commands
            .run(here, &["git", "--version"], &log)
            .await
            .unwrap();
        let err = Commands
            .run(here, &["git", "no-such-command"], &log)
            .await
            .unwrap_err();
        assert!(err.contains("`git no-such-command` failed"), "{err}");
        let lines = lines.into_inner().unwrap();
        assert!(lines[0].starts_with("git version"), "{lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("no-such-command")),
            "{lines:?}"
        );
    }
}
