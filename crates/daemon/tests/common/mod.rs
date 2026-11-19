//! Test harness: runs the daemon in-process on ephemeral ports.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use ferrobot_daemon::{AppState, Settings, agent_router, ui_router};
use ferrobot_sandbox::SandboxBackend;
use ferrobot_store::Store;
use serde_json::Value;

pub struct Daemon {
    pub state: AppState,
    pub ui: String,
    pub agents: String,
}

pub async fn start(
    backend: Arc<dyn SandboxBackend>,
    sandbox_url: Option<String>,
    agent_bind: &str,
) -> Daemon {
    let agents_listener = tokio::net::TcpListener::bind(agent_bind).await.unwrap();
    let agents_addr = agents_listener.local_addr().unwrap();
    let ui_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ui_addr = ui_listener.local_addr().unwrap();
    let state = AppState::new(
        Store::open_in_memory().unwrap(),
        backend,
        Settings {
            sandbox_daemon_url: sandbox_url
                .unwrap_or_else(|| format!("http://127.0.0.1:{}", agents_addr.port()))
                .replace("{port}", &agents_addr.port().to_string()),
            driver: "mock".into(),
            anthropic_api_key: None,
            model: None,
            turn_timeout: Duration::from_secs(30),
        },
    );
    let ui = ui_router(state.clone());
    let agents = agent_router(state.clone());
    tokio::spawn(async move { axum::serve(ui_listener, ui).await.unwrap() });
    tokio::spawn(async move { axum::serve(agents_listener, agents).await.unwrap() });
    Daemon {
        state,
        ui: format!("127.0.0.1:{}", ui_addr.port()),
        agents: format!("127.0.0.1:{}", agents_addr.port()),
    }
}

/// Tiny HTTP/1.1 JSON client (keeps the test dependency tree small).
pub async fn http(addr: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    let (_, raw) = text.split_once("\r\n\r\n").unwrap_or_default();
    let raw = if text
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(raw)
    } else {
        raw.to_owned()
    };
    (status, serde_json::from_str(&raw).unwrap_or(Value::Null))
}

fn dechunk(mut raw: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = raw.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.push_str(&rest[..n]);
        raw = &rest[n + 2..];
    }
    out
}

/// Polls `f` until it returns `Some` or the timeout elapses.
pub async fn eventually<T, F, Fut>(what: &str, timeout: Duration, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Event kinds of an agent, oldest first.
pub async fn events(d: &Daemon, agent_id: &str) -> Vec<Value> {
    let (status, v) = http(&d.ui, "GET", &format!("/v1/agents/{agent_id}/events"), None).await;
    assert_eq!(status, 200);
    v.as_array().cloned().unwrap_or_default()
}
