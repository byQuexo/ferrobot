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
    start_with(backend, sandbox_url, agent_bind, |_| {}).await
}

/// Like [`start`], with a hook to adjust the settings.
pub async fn start_with(
    backend: Arc<dyn SandboxBackend>,
    sandbox_url: Option<String>,
    agent_bind: &str,
    configure: impl FnOnce(&mut Settings),
) -> Daemon {
    start_on(
        Store::open_in_memory().unwrap(),
        backend,
        sandbox_url,
        agent_bind,
        configure,
    )
    .await
}

/// Like [`start_with`], on a given store (e.g. a file, to test restarts).
pub async fn start_on(
    store: Store,
    backend: Arc<dyn SandboxBackend>,
    sandbox_url: Option<String>,
    agent_bind: &str,
    configure: impl FnOnce(&mut Settings),
) -> Daemon {
    let agents_listener = tokio::net::TcpListener::bind(agent_bind).await.unwrap();
    let agents_addr = agents_listener.local_addr().unwrap();
    let ui_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ui_addr = ui_listener.local_addr().unwrap();
    let mut settings = Settings::new(
        sandbox_url
            .unwrap_or_else(|| format!("http://127.0.0.1:{}", agents_addr.port()))
            .replace("{port}", &agents_addr.port().to_string()),
    );
    settings.turn_timeout = Duration::from_secs(30);
    configure(&mut settings);
    let state = AppState::new(store, backend, settings);
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
    let value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    if path.starts_with("/v1/") {
        assert_conforms(method, path, status, &value);
    }
    (status, value)
}

/// Raw HTTP/1.1 request with arbitrary bytes; returns status, lower-cased
/// headers and the (de-chunked) body bytes.
pub async fn http_raw(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, Vec<(String, String)>, Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        use std::fmt::Write as _;
        let _ = write!(req, "{k}: {v}\r\n");
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&buf[..split]).into_owned();
    let mut raw = buf[split + 4..].to_vec();
    let mut lines = head.lines();
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    if headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.contains("chunked"))
    {
        raw = dechunk_bytes(&raw);
    }
    (status, headers, raw)
}

fn dechunk_bytes(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(pos) = raw.windows(2).position(|w| w == b"\r\n") {
        let n = usize::from_str_radix(String::from_utf8_lossy(&raw[..pos]).trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        let rest = &raw[pos + 2..];
        out.extend_from_slice(&rest[..n]);
        raw = &rest[n + 2..];
    }
    out
}

/// Checks a UI API response against the generated `OpenAPI` document: the status
/// must be documented for the route, and a JSON body must match its schema.
pub fn assert_conforms(method: &str, path: &str, status: u16, body: &Value) {
    let spec = serde_json::to_value(ferrobot_daemon::spec::openapi()).unwrap();
    let path = path.split('?').next().unwrap();
    let segments: Vec<&str> = path.split('/').collect();
    let (_, item) = spec["paths"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(template, _)| {
            let t: Vec<&str> = template.split('/').collect();
            t.len() == segments.len()
                && t.iter()
                    .zip(&segments)
                    .all(|(a, b)| a == b || a.starts_with('{'))
        })
        .unwrap_or_else(|| panic!("{path} is not in the OpenAPI document"));
    let op = &item[method.to_ascii_lowercase()];
    assert!(op.is_object(), "{method} {path} is not documented");
    let response = &op["responses"][status.to_string()];
    assert!(
        response.is_object(),
        "{method} {path} returned undocumented status {status}: {body}"
    );
    let Some(schema) = response["content"]["application/json"].get("schema") else {
        return;
    };
    let root = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "allOf": [schema],
        "components": spec["components"],
    });
    let validator = jsonschema::validator_for(&root).unwrap();
    let errors: Vec<String> = validator.iter_errors(body).map(|e| e.to_string()).collect();
    assert!(
        errors.is_empty(),
        "{method} {path} ({status}) does not match OpenAPI: {errors:?}\nbody: {body}"
    );
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
