//! `ferrobotd`: the ferrobot daemon binary.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use ferrobot_daemon::{AppState, Settings, agent_router, lifecycle, ui_router};
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
    /// Claude via the Claude Agent SDK (needs ANTHROPIC_API_KEY).
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
    #[arg(long, env = "FERROBOT_RUNNER_IMAGE", default_value = "ferrobot-runner:dev")]
    runner_image: String,
    /// Runner entry point for the local backend.
    #[arg(long, env = "FERROBOT_RUNNER_ENTRY", default_value = "runner/dist/index.js")]
    runner_entry: PathBuf,
    /// Seconds before a turn is interrupted and its message retried.
    #[arg(long, env = "FERROBOT_TURN_TIMEOUT", default_value_t = 600)]
    turn_timeout: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ferrobot=debug")),
        )
        .init();
    let cli = Cli::parse();

    std::fs::create_dir_all(&cli.data_dir)
        .with_context(|| format!("creating {}", cli.data_dir.display()))?;
    let store = Store::open(cli.data_dir.join("ferrobot.db")).context("opening database")?;

    let backend: Arc<dyn SandboxBackend> = match cli.backend {
        Backend::Docker => {
            let docker = DockerBackend::connect(DockerConfig {
                image: cli.runner_image.clone(),
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

    let anthropic_api_key = std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.is_empty());
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

    let state = AppState::new(
        store,
        backend,
        Settings {
            sandbox_daemon_url,
            driver: cli.driver.as_str().into(),
            anthropic_api_key,
            model: cli.model.clone(),
            turn_timeout: Duration::from_secs(cli.turn_timeout),
        },
    );

    let ui = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("binding UI API on {}", cli.listen))?;
    let agents = tokio::net::TcpListener::bind(cli.agent_listen)
        .await
        .with_context(|| format!("binding agent API on {}", cli.agent_listen))?;
    tracing::info!(ui = %cli.listen, agents = %cli.agent_listen, backend = ?cli.backend, driver = ?cli.driver, "ferrobotd listening");

    lifecycle::reconcile(&state).await;

    let ui_server = axum::serve(ui, ui_router(state.clone())).with_graceful_shutdown(shutdown());
    let agent_server = axum::serve(agents, agent_router(state)).with_graceful_shutdown(shutdown());
    tokio::try_join!(ui_server, agent_server).context("server error")?;
    tracing::info!("bye");
    Ok(())
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = term => {} }
}
