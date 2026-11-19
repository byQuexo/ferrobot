//! Drives the MCP server with the official rmcp client over real HTTP.

use std::sync::{Arc, Mutex};

use ferrobot_core::{AgentId, AgentName, MailMessage, MailStatus};
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
    fn message_enqueued(&self, message: &MailMessage) {
        self.0.lock().unwrap().push((message.recipient, message.id));
    }

    fn request_agent(
        &self,
        _requester: AgentId,
        request: ferrobot_mcp::AgentRequest,
    ) -> ferrobot_mcp::BoxFuture<Result<ferrobot_mcp::AgentRequestOutcome, String>> {
        Box::pin(async move {
            match request.name.as_str() {
                "taken" => Err("agent name taken is taken".into()),
                "nope" => Ok(ferrobot_mcp::AgentRequestOutcome::Declined {
                    reason: "the operator declined".into(),
                }),
                _ => Ok(ferrobot_mcp::AgentRequestOutcome::Created {
                    id: AgentId::new(),
                    name: request.name,
                }),
            }
        })
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
#[allow(clippy::too_many_lines, reason = "one end-to-end scenario")]
async fn tools_act_as_the_authenticated_agent() {
    let store = Store::open_in_memory().unwrap();
    let mut ids = Vec::new();
    for (name, token) in [("alice", "tok-a"), ("bob", "tok-b")] {
        let a = store
            .create_agent(NewAgent {
                created_by: None,
                name: AgentName::parse(name).unwrap(),
                description: String::new(),
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
            "ask_user",
            "create_agent",
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
    assert_eq!(sent["agent_id"], ids[1].to_string());
    let msg_id = sent["message_id"].as_i64().unwrap();
    assert_eq!(*recorder.0.lock().unwrap(), [(ids[1], msg_id)]);

    // Ids (as listed by list_agents) address agents too.
    let bob_id = list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "bob")
        .map(|a| a["id"].as_str().unwrap().to_owned())
        .unwrap();
    assert_eq!(bob_id, ids[1].to_string());
    let sent = call(
        &alice,
        "send_message",
        json!({"to": bob_id, "body": "by id"}),
    )
    .await
    .unwrap();
    assert_eq!(sent["queued_for"], "bob");
    recorder.0.lock().unwrap().pop();

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

    // bob answers in the thread, urgently; the mention is stored by id and
    // read back with the current name
    let reply = call(
        &bob,
        "send_message",
        json!({"to": "alice", "body": "thanks @Alice", "reply_to": msg_id, "priority": true}),
    )
    .await
    .unwrap();
    let stored = store
        .get_message(reply["message_id"].as_i64().unwrap())
        .await
        .unwrap();
    assert_eq!(stored.body, format!("thanks <@{}>", ids[0]));
    assert_eq!(
        (stored.reply_to, stored.priority),
        (Some(msg_id), Some(true))
    );
    let inbox = call(&alice, "read_mailbox", json!({})).await.unwrap();
    assert_eq!(inbox["messages"][0]["body"], "thanks @alice");
    // ... but cannot reply to mail it was not part of
    let foreign = store
        .enqueue_message(ferrobot_core::Sender::User, ids[0], "private".into(), None)
        .await
        .unwrap();
    assert!(
        call(
            &bob,
            "send_message",
            json!({"to": "alice", "body": "x", "reply_to": foreign.id})
        )
        .await
        .is_err()
    );

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

    // Question cards are validated and recorded in the caller's own chat.
    let card = json!({"questions": [
        {"prompt": "Ship it?", "kind": "single", "choices": [{"label": "Yes"}, {"label": "No"}]},
        {"prompt": "Notes", "kind": "text"}
    ]});
    let asked = call(&alice, "ask_user", card).await.unwrap();
    let qid = asked["question_id"].as_i64().unwrap();
    let events = store.events_after(ids[0], 0, 10).await.unwrap();
    let q = events.iter().find(|e| e.id == qid).unwrap();
    assert_eq!(q.kind, "question");
    assert_eq!(q.payload["version"], 1);
    assert_eq!(q.payload["card"]["questions"][1]["id"], "q2");
    assert_eq!(
        q.payload["card"]["questions"][0]["choices"][0]["value"],
        "Yes"
    );
    let bad =
        json!({"questions": [{"prompt": "?", "kind": "single", "choices": [{"label": "only"}]}]});
    assert!(call(&alice, "ask_user", bad).await.is_err());

    let made = call(
        &alice,
        "create_agent",
        json!({"name": "scout", "description": "Scouts"}),
    )
    .await
    .unwrap();
    assert_eq!(made["status"], "created");
    assert!(made["agent_id"].is_string());
    let no = call(
        &alice,
        "create_agent",
        json!({"name": "nope", "description": "x"}),
    )
    .await
    .unwrap();
    assert_eq!(no["status"], "declined");
    assert!(no["agent_id"].is_null());
    assert!(
        call(
            &alice,
            "create_agent",
            json!({"name": "taken", "description": "x"})
        )
        .await
        .is_err()
    );

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
