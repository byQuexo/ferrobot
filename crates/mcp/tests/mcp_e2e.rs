//! Drives the MCP server with the official rmcp client over real HTTP.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::{Arc, Mutex};

use ferrobot_core::{AgentId, AgentName, MailMessage, MailStatus, Sender};
use ferrobot_mcp::{MailboxNotifier, McpState};
use ferrobot_store::{NewAgent, Store};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};

/// Enqueued mail, and the existing agent that `create_agent` "creates".
#[derive(Default)]
struct Recorder(Mutex<Vec<(AgentId, i64)>>, Mutex<Option<AgentId>>);

impl MailboxNotifier for Recorder {
    fn message_enqueued(&self, message: &MailMessage) {
        self.0.lock().unwrap().push((message.recipient, message.id));
    }

    fn request_agent(
        &self,
        _requester: AgentId,
        request: ferrobot_mcp::AgentRequest,
    ) -> ferrobot_mcp::BoxFuture<Result<ferrobot_mcp::AgentRequestOutcome, String>> {
        let existing = *self.1.lock().unwrap();
        Box::pin(async move {
            match request.name.as_str() {
                "taken" => Err("agent name taken is taken".into()),
                "nope" => Ok(ferrobot_mcp::AgentRequestOutcome::Declined {
                    reason: "the operator declined".into(),
                }),
                _ => Ok(ferrobot_mcp::AgentRequestOutcome::Created {
                    id: existing.unwrap_or_default(),
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
            "add_connector",
            "ask_user",
            "assign_task",
            "call_agents",
            "create_agent",
            "create_routine",
            "forget",
            "hang_up",
            "invite_to_call",
            "list_agents",
            "list_guides",
            "list_tasks",
            "react",
            "read_guide",
            "read_mailbox",
            "recall",
            "remember",
            "send_message",
            "set_agent_tools",
            "share_file"
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

    // Global memory is shared, and recall says where a hit came from.
    call(
        &alice,
        "remember",
        json!({"key": "home", "content": "Operator flies from Hamburg", "global": true}),
    )
    .await
    .unwrap();
    call(
        &bob,
        "remember",
        json!({"key": "home", "content": "bob's own Hamburg note"}),
    )
    .await
    .unwrap();
    let hits = call(&bob, "recall", json!({"query": "hamburg"}))
        .await
        .unwrap();
    let scopes: Vec<_> = hits["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["content"].as_str().unwrap(),
                m["global"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        scopes,
        [
            ("bob's own Hamburg note", false),
            ("Operator flies from Hamburg", true)
        ]
    );
    assert_eq!(hits["memories"][1]["written_by"], "alice");
    let planted = call(
        &bob,
        "remember",
        json!({"key": "home", "content": "Operator flies from Paris", "global": true}),
    )
    .await;
    assert!(
        planted.is_err(),
        "bob cannot overwrite alice's global memory"
    );
    let limited = call(&bob, "recall", json!({"query": "hamburg", "limit": 1}))
        .await
        .unwrap();
    assert_eq!(limited["memories"].as_array().unwrap().len(), 1);
    let gone = call(&alice, "forget", json!({"key": "home", "global": true}))
        .await
        .unwrap();
    assert_eq!(gone["ok"], true);
    assert_eq!(store.recall_global(String::new(), 5).await.unwrap(), []);

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

    // Reactions go only on the operator's own messages to the caller.
    let to_alice = store
        .enqueue_message(Sender::User, ids[0], "Book the train".into(), None)
        .await
        .unwrap();
    let to_bob = store
        .enqueue_message(Sender::User, ids[1], "Hi bob".into(), None)
        .await
        .unwrap();
    let react = |id: i64, emoji: &str| json!({"message_id": id, "emoji": emoji});
    call(&alice, "react", react(to_alice.id, " 👍 "))
        .await
        .unwrap();
    let events = store.events_after(ids[0], 0, 20).await.unwrap();
    let r = events.iter().rfind(|e| e.kind == "reaction").unwrap();
    assert_eq!(r.payload, json!({"message_id": to_alice.id, "emoji": "👍"}));
    assert!(call(&alice, "react", react(to_bob.id, "👍")).await.is_err());
    assert!(call(&alice, "react", react(9999, "👍")).await.is_err());
    for bad in ["", "ok", "👍 👍", &"👍".repeat(9)] {
        let refused = call(&alice, "react", react(to_alice.id, bad)).await;
        assert!(refused.is_err(), "{bad:?}");
    }

    let made = call(
        &alice,
        "create_agent",
        json!({"name": "scout", "description": "Scouts"}),
    )
    .await
    .unwrap();
    assert_eq!(made["status"], "created");
    assert!(made["agent_id"].is_string());
    assert!(made["note"].as_str().unwrap().contains("idle"), "{made}");

    // A task is sent as the creator's first message to the new agent.
    *recorder.1.lock().unwrap() = Some(ids[1]);
    recorder.0.lock().unwrap().clear();
    let briefed = call(
        &alice,
        "create_agent",
        json!({"name": "helper", "description": "Helps", "task": " Plan the trip, ask @alice. "}),
    )
    .await
    .unwrap();
    let brief = store.inbox(ids[1], 1).await.unwrap().pop().unwrap();
    assert_eq!(*recorder.0.lock().unwrap(), [(ids[1], brief.id)]);
    assert_eq!(brief.sender, Sender::Agent(ids[0]));
    assert_eq!(brief.body, format!("Plan the trip, ask <@{}>.", ids[0]));
    assert!(
        briefed["note"]
            .as_str()
            .unwrap()
            .contains(&format!("message #{}", brief.id)),
        "{briefed}"
    );
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

    // An agent created by alice asks through alice, not with a card.
    let kid = store
        .create_agent(NewAgent {
            created_by: Some(ids[0]),
            name: AgentName::parse("kid").unwrap(),
            description: String::new(),
            instructions: String::new(),
        })
        .await
        .unwrap();
    store.set_agent_token(kid.id, "tok-k").await.unwrap();
    let as_kid = client(&url, "tok-k").await;
    let card = json!({"questions": [
        {"prompt": "Budget?", "kind": "single", "choices": [{"label": "Low"}, {"label": "High"}]}
    ]});
    let asked = call(&as_kid, "ask_user", card).await.unwrap();
    let relayed = store.inbox(ids[0], 1).await.unwrap().pop().unwrap();
    assert_eq!(asked["question_id"], relayed.id);
    assert_eq!(relayed.sender, Sender::Agent(kid.id));
    assert!(
        relayed.body.contains("1. Budget?\n   Pick one: Low / High"),
        "{}",
        relayed.body
    );
    let kid_events = store.events_after(kid.id, 0, 10).await.unwrap();
    assert!(
        kid_events.iter().all(|e| e.kind != "question"),
        "no card in kid's chat"
    );

    as_kid.cancel().await.unwrap();
    alice.cancel().await.unwrap();
    bob.cancel().await.unwrap();
}

#[tokio::test]
async fn tools_outside_the_profile_are_hidden_and_refused() {
    use ferrobot_core::ToolGroup;
    let store = Store::open_in_memory().unwrap();
    let a = store
        .create_agent(NewAgent {
            created_by: None,
            name: AgentName::parse("carl").unwrap(),
            description: String::new(),
            instructions: String::new(),
        })
        .await
        .unwrap();
    store.set_agent_token(a.id, "tok-c").await.unwrap();
    let patch = ferrobot_store::AgentPatch {
        tools: Some(vec![ToolGroup::Memory, ToolGroup::Shell]),
        ..Default::default()
    };
    store.update_agent_profile(a.id, patch).await.unwrap();
    let url = serve(McpState {
        store: store.clone(),
        notifier: Arc::new(Recorder::default()),
    })
    .await;

    let carl = client(&url, "tok-c").await;
    let mut names: Vec<_> = carl
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["forget", "recall", "remember"]);
    call(&carl, "remember", json!({"key": "k", "content": "v"}))
        .await
        .unwrap();
    // Unlisted is not enough: a model can call a tool by name.
    let err = call(&carl, "list_agents", json!({})).await.unwrap_err();
    assert!(err.contains("list_agents is turned off for you"), "{err}");
    let err = call(
        &carl,
        "create_agent",
        json!({"name": "x", "description": "y"}),
    )
    .await
    .unwrap_err();
    assert!(err.contains("turned off"), "{err}");
    carl.cancel().await.unwrap();
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
