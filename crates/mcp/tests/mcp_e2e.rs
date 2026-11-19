//! Drives the MCP server with the official rmcp client over real HTTP.

use std::sync::{Arc, Mutex};

use ferrobot_core::{AgentId, AgentName, MailStatus};
use ferrobot_mcp::{MailboxNotifier, McpState};
use ferrobot_store::{NewAgent, Store};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};

#[derive(Default)]
struct Recorder(Mutex<Vec<(AgentId, i64)>>);

impl MailboxNotifier for Recorder {
    fn message_enqueued(&self, recipient: AgentId, message_id: i64) {
        self.0.lock().unwrap().push((recipient, message_id));
    }
}

async fn serve(state: McpState) -> String {
    let app = axum::Router::new().nest("/mcp", ferrobot_mcp::router(state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/mcp")
}

async fn client(url: &str, token: &str) -> RunningService<RoleClient, ()> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url).auth_header(token),
    );
    ().serve(transport).await.expect("mcp handshake")
}

async fn call(
    c: &RunningService<RoleClient, ()>,
    tool: &'static str,
    args: Value,
) -> Result<Value, String> {
    let params = CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone());
    match c.call_tool(params).await {
        Ok(r) if r.is_error != Some(true) => Ok(r.structured_content.unwrap_or(Value::Null)),
        Ok(r) => Err(format!("{:?}", r.content)),
        Err(e) => Err(e.to_string()),
    }
}

#[tokio::test]
async fn tools_act_as_the_authenticated_agent() {
    let store = Store::open_in_memory().unwrap();
    let mut ids = Vec::new();
    for (name, token) in [("alice", "tok-a"), ("bob", "tok-b")] {
        let a = store
            .create_agent(NewAgent {
                name: AgentName::parse(name).unwrap(),
                instructions: format!("{name} role"),
            })
            .await
            .unwrap();
        store.set_agent_token(a.id, token).await.unwrap();
        ids.push(a.id);
    }
    let recorder = Arc::new(Recorder::default());
    let url = serve(McpState {
        store: store.clone(),
        notifier: recorder.clone(),
    })
    .await;

    let alice = client(&url, "tok-a").await;
    let tools = alice.list_all_tools().await.unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "forget",
            "list_agents",
            "read_mailbox",
            "recall",
            "remember",
            "send_message"
        ]
    );

    let list = call(&alice, "list_agents", json!({})).await.unwrap();
    let me: Vec<_> = list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["is_you"] == true)
        .map(|a| a["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(me, ["alice"]);

    let sent = call(
        &alice,
        "send_message",
        json!({"to": "@bob", "body": "hi bob"}),
    )
    .await
    .unwrap();
    assert_eq!(sent["queued_for"], "bob");
    let msg_id = sent["message_id"].as_i64().unwrap();
    assert_eq!(*recorder.0.lock().unwrap(), [(ids[1], msg_id)]);

    // validation errors surface as tool errors, not panics
    assert!(
        call(&alice, "send_message", json!({"to": "alice", "body": "me"}))
            .await
            .is_err()
    );
    assert!(
        call(&alice, "send_message", json!({"to": "nobody", "body": "x"}))
            .await
            .is_err()
    );
    assert!(
        call(&alice, "send_message", json!({"to": "bob", "body": "   "}))
            .await
            .is_err()
    );

    // bob sees alice's message with a resolved sender name
    let bob = client(&url, "tok-b").await;
    let inbox = call(&bob, "read_mailbox", json!({})).await.unwrap();
    assert_eq!(inbox["messages"][0]["from"], "alice");
    assert_eq!(inbox["messages"][0]["body"], "hi bob");
    let queued = store.inbox(ids[1], 10).await.unwrap();
    assert_eq!(queued[0].status, MailStatus::Pending);

    // memory is private per agent
    call(
        &alice,
        "remember",
        json!({"key": "k", "content": "alice secret about rust"}),
    )
    .await
    .unwrap();
    let hits = call(&alice, "recall", json!({"query": "rust"}))
        .await
        .unwrap();
    assert_eq!(hits["memories"].as_array().unwrap().len(), 1);
    let none = call(&bob, "recall", json!({"query": "rust"}))
        .await
        .unwrap();
    assert_eq!(none["memories"], json!([]));
    let gone = call(&alice, "forget", json!({"key": "k"})).await.unwrap();
    assert_eq!(gone["ok"], true);

    alice.cancel().await.unwrap();
    bob.cancel().await.unwrap();
}

#[tokio::test]
async fn rejects_missing_or_unknown_tokens() {
    let store = Store::open_in_memory().unwrap();
    let url = serve(McpState {
        store,
        notifier: Arc::new(Recorder::default()),
    })
    .await;
    let http = reqwest_free_post(&url, None).await;
    assert_eq!(http, 401);
    let http = reqwest_free_post(&url, Some("nope")).await;
    assert_eq!(http, 401);
}

/// Minimal raw HTTP POST so the test does not depend on a specific HTTP client crate.
async fn reqwest_free_post(url: &str, token: Option<&str>) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_once('/').unwrap();
    let mut s = tokio::net::TcpStream::connect(host).await.unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let body = "{}";
    let req = format!(
        "POST /{path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    text.split_whitespace().nth(1).unwrap().parse().unwrap()
}
