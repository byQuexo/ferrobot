//! `ferrobotd`: the ferrobot daemon binary.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use ferrobot_daemon::{
    AppState, Settings, agent_router, lifecycle, scheduler, storage, supervisor, ui_router,
};
use ferrobot_sandbox::{
    DockerBackend, DockerConfig, LocalProcessBackend, LocalProcessConfig, SandboxBackend,
};
use ferrobot_store::Store;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Backend {
    /// One container per agent (Docker or Podman socket).
    Docker,
    /// Runner as a host child process. No isolation, development only.
    Local,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Driver {
    /// Deterministic fake model, no API key needed.
    Mock,
    /// Claude via the Claude Agent SDK (needs `ANTHROPIC_API_KEY`).
    Claude,
}

impl Driver {
    fn as_str(self) -> &'static str {
        match self {
            Self::Mock => "mock",
            Self::Claude => "claude",
        }
    }
}

/// ferrobot daemon: one computer per agent.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Directory for the database and local sandboxes.
    #[arg(long, env = "FERROBOT_DATA_DIR", default_value = ".ferrobot")]
    data_dir: PathBuf,
    /// UI API address. Keep it on loopback: it is not authenticated.
    #[arg(long, env = "FERROBOT_LISTEN", default_value = "127.0.0.1:7878")]
    listen: SocketAddr,
    /// Agent API address (runner WebSocket + MCP, token-authenticated).
    /// With Docker it must be reachable from containers, e.g. 0.0.0.0:7879.
    #[arg(long, env = "FERROBOT_AGENT_LISTEN", default_value = "127.0.0.1:7879")]
    agent_listen: SocketAddr,
    /// Agent API URL as seen from inside a sandbox (derived if omitted).
    #[arg(long, env = "FERROBOT_SANDBOX_DAEMON_URL")]
    sandbox_daemon_url: Option<String>,
    #[arg(long, env = "FERROBOT_BACKEND", value_enum, default_value_t = Backend::Docker)]
    backend: Backend,
    #[arg(long, env = "FERROBOT_DRIVER", value_enum, default_value_t = Driver::Mock)]
    driver: Driver,
    /// Optional model override passed to the runner.
    #[arg(long, env = "FERROBOT_MODEL")]
    model: Option<String>,
    /// Runner image for the Docker backend.
    #[arg(
        long,
        env = "FERROBOT_RUNNER_IMAGE",
        default_value = "ferrobot-runner:dev"
    )]
    runner_image: String,
    /// Object storage for snapshot archives: `s3://bucket/prefix` (credentials
    /// and endpoint from `AWS_*`, e.g. Cloudflare R2 via `AWS_ENDPOINT`) or
    /// `file:///dir`. Default: the data directory.
    #[arg(long, env = "FERROBOT_STORAGE_URL")]
    storage_url: Option<String>,
    /// Runner entry point for the local backend.
    #[arg(
        long,
        env = "FERROBOT_RUNNER_ENTRY",
        default_value = "runner/dist/index.js"
    )]
    runner_entry: PathBuf,
    /// Seconds before a turn is interrupted and its message retried.
    #[arg(long, env = "FERROBOT_TURN_TIMEOUT", default_value_t = 600)]
    turn_timeout: u64,
    /// Seconds without activity before an agent's sandbox goes to sleep
    /// (0 disables). Messages and routines wake it again.
    #[arg(long, env = "FERROBOT_IDLE_TIMEOUT", default_value_t = 900)]
    idle_timeout: u64,
    /// Default IANA time zone for routines (default: the system's).
    #[arg(long, env = "FERROBOT_TIMEZONE")]
    timezone: Option<String>,
    /// Upstream for the model proxy.
    #[arg(
        long,
        env = "FERROBOT_ANTHROPIC_UPSTREAM",
        default_value = "https://api.anthropic.com"
    )]
    anthropic_upstream: String,
    /// Bearer token for the UI API. Default: generated on first start and
    /// stored in `<data dir>/api-token` (mode 0600).
    #[arg(long, env = "FERROBOT_API_TOKEN", hide_env_values = true)]
    api_token: Option<String>,
    /// Most agents this host keeps; applies to the API and to agents
    /// creating agents.
    #[arg(long, env = "FERROBOT_MAX_AGENTS", default_value_t = ferrobot_daemon::agents::DEFAULT_MAX_AGENTS)]
    max_agents: usize,
    /// Seconds an agent's `create_agent` request waits for the operator's
    /// approval before it counts as declined.
    #[arg(long, env = "FERROBOT_APPROVAL_TIMEOUT", default_value_t = 300)]
    approval_timeout: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,ferrobot=debug")),
        )
        .init();
    let cli = Cli::parse();

    std::fs::create_dir_all(&cli.data_dir)
        .with_context(|| format!("creating {}", cli.data_dir.display()))?;
    let store = Store::open(cli.data_dir.join("ferrobot.db")).context("opening database")?;

    let backend = make_backend(&cli).await?;
    let api_token = api_token(&cli)?;

    let anthropic_api_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.is_empty());
    if matches!(cli.driver, Driver::Claude) && anthropic_api_key.is_none() {
        bail!("--driver claude needs ANTHROPIC_API_KEY in the daemon environment");
    }
    let sandbox_daemon_url = cli.sandbox_daemon_url.clone().unwrap_or_else(|| {
        let host = match cli.backend {
            Backend::Docker => "host.docker.internal",
            Backend::Local => "127.0.0.1",
        };
        format!("http://{host}:{}", cli.agent_listen.port())
    });

    let default_timezone = cli
        .timezone
        .clone()
        .unwrap_or_else(scheduler::system_timezone);
    scheduler::Schedule::parse("@daily", &default_timezone)
        .map_err(|e| anyhow::anyhow!("--timezone: {e}"))?;
    let mut settings = Settings::new(sandbox_daemon_url);
    settings.driver = cli.driver.as_str().into();
    settings.anthropic_api_key = anthropic_api_key;
    settings.anthropic_upstream = cli.anthropic_upstream.trim_end_matches('/').to_owned();
    settings.model = cli.model.clone();
    settings.turn_timeout = Duration::from_secs(cli.turn_timeout);
    settings.idle_timeout = (cli.idle_timeout > 0).then(|| Duration::from_secs(cli.idle_timeout));
    // Check often enough that short idle timeouts are honoured promptly.
    if let Some(idle) = settings.idle_timeout {
        settings.supervisor_interval =
            (idle / 2).clamp(Duration::from_secs(1), Duration::from_secs(15));
    }
    settings.default_timezone = default_timezone;
    settings.api_token = Some(api_token);
    settings.max_agents = cli.max_agents;
    settings.approval_timeout = Duration::from_secs(cli.approval_timeout);
    let state = AppState::new(store, backend, settings);

    let ui = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("binding UI API on {}", cli.listen))?;
    let agents = tokio::net::TcpListener::bind(cli.agent_listen)
        .await
        .with_context(|| format!("binding agent API on {}", cli.agent_listen))?;
    tracing::info!(ui = %cli.listen, agents = %cli.agent_listen, backend = ?cli.backend, driver = ?cli.driver, "ferrobotd listening");

    lifecycle::reconcile(&state).await;
    tokio::spawn(scheduler::run(state.clone()));
    tokio::spawn(supervisor::run(state.clone()));

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown().await;
        let _ = stop_tx.send(true);
    });
    let stopped = |mut rx: tokio::sync::watch::Receiver<bool>| async move {
        let _ = rx.wait_for(|stop| *stop).await;
    };
    let ui_server =
        axum::serve(ui, ui_router(state.clone())).with_graceful_shutdown(stopped(stop_rx.clone()));
    let agent_server =
        axum::serve(agents, agent_router(state)).with_graceful_shutdown(stopped(stop_rx.clone()));
    // WebSockets (runners, UI) never finish on their own, so graceful shutdown
    // gets a grace period; leaving `main` then drops them and runners reconnect
    // to the next daemon.
    let grace = async {
        stopped(stop_rx).await;
        tokio::time::sleep(SHUTDOWN_GRACE).await;
        tracing::info!("closing remaining connections");
    };
    tokio::select! {
        result = async { tokio::try_join!(ui_server, agent_server) } => {
            result.context("server error")?;
        }
        () = grace => {}
    }
    tracing::info!("bye");
    Ok(())
}

/// The operator token: from `--api-token`, else the token file (created on first start).
fn api_token(cli: &Cli) -> Result<String> {
    if let Some(token) = cli.api_token.as_deref().filter(|t| !t.is_empty()) {
        ferrobot_daemon::auth::validate(token).map_err(|e| anyhow::anyhow!("--api-token: {e}"))?;
        return Ok(token.to_owned());
    }
    let token = ferrobot_daemon::auth::load_or_create(&cli.data_dir).context("API token file")?;
    tracing::info!(
        file = %ferrobot_daemon::auth::token_path(&cli.data_dir).display(),
        "UI API token (send as `Authorization: Bearer <token>`)"
    );
    Ok(token)
}

async fn make_backend(cli: &Cli) -> Result<Arc<dyn SandboxBackend>> {
    let backend: Arc<dyn SandboxBackend> = match cli.backend {
        Backend::Docker => {
            let docker = DockerBackend::connect(DockerConfig {
                image: cli.runner_image.clone(),
                snapshot_store: Some(
                    storage::open(cli.storage_url.as_deref(), &cli.data_dir)
                        .context("--storage-url")?,
                ),
                ..DockerConfig::default()
            })?;
            docker
                .ping()
                .await
                .context("Docker/Podman engine not reachable (is the daemon running?)")?;
            if cli.agent_listen.ip().is_loopback() {
                tracing::warn!(
                    "agent API listens on loopback; containers cannot reach it. \
                     Use --agent-listen 0.0.0.0:{} (token-authenticated)",
                    cli.agent_listen.port()
                );
            }
            Arc::new(docker)
        }
        Backend::Local => {
            let entry = std::fs::canonicalize(&cli.runner_entry).with_context(|| {
                format!(
                    "runner entry {} not found; build it with `npm --prefix runner run build`",
                    cli.runner_entry.display()
                )
            })?;
            tracing::warn!("local backend: agents run as host processes WITHOUT isolation");
            Arc::new(LocalProcessBackend::new(LocalProcessConfig {
                root: cli.data_dir.join("sandboxes"),
                runner_cmd: vec!["node".into(), entry.display().to_string()],
            })?)
        }
    };
    Ok(backend)
}

const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = term => {} }
}
