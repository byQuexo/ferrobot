//! End-to-end: real daemon, TypeScript runner (mock driver) and sandbox backend; two agents talk
//! through the mailbox and MCP. Run with `-- --ignored` (commands in CLAUDE.md).

#![allow(
    clippy::too_many_lines,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test scenarios read best as one story"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Daemon, events, eventually, http, start};
use ferrobot_sandbox::{LocalProcessBackend, LocalProcessConfig, SandboxBackend};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(60);

async fn create(d: &Daemon, name: &str) -> String {
    let (status, agent) = http(
        &d.ui,
        "POST",
        "/v1/agents",
        Some(json!({"name": name, "instructions": format!("{name} test agent")})),
    )
    .await;
    assert_eq!(status, 201, "{agent}");
    agent["id"].as_str().unwrap().to_owned()
}

async fn wait_connected(d: &Daemon, id: &str) {
    eventually("runner connected", WAIT, || async {
        let (_, a) = http(&d.ui, "GET", &format!("/v1/agents/{id}"), None).await;
        (a["connected"] == true).then_some(())
    })
    .await;
}

async fn wait_event(d: &Daemon, id: &str, pred: impl Fn(&Value) -> bool) -> Value {
    eventually("matching event", WAIT, || async {
        events(d, id).await.into_iter().find(|e| pred(e))
    })
    .await
}

async fn two_agents_talk(d: Daemon) {
    let alice = create(&d, "alice").await;
    let bob = create(&d, "bob").await;
    wait_connected(&d, &alice).await;
    wait_connected(&d, &bob).await;

    // user -> alice: "@bob ..." makes alice call send_message(bob) over MCP
    let (status, _) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{alice}/messages"),
        Some(json!({"body": "@bob hello from alice"})),
    )
    .await;
    assert_eq!(status, 202);

    // bob receives it from alice ...
    wait_event(&d, &bob, |e| {
        e["kind"] == "message_in"
            && e["payload"]["from"] == "alice"
            && e["payload"]["body"] == "hello from alice"
    })
    .await;
    // ... and the mock answers via send_message back to alice
    wait_event(&d, &alice, |e| {
        e["kind"] == "message_in"
            && e["payload"]["from"] == "bob"
            && e["payload"]["body"] == "re: hello from alice"
    })
    .await;
    wait_event(&d, &alice, |e| {
        e["kind"] == "turn_done"
            && e["payload"]["outcome"] == "completed"
            && e["payload"]["result"]
                .as_str()
                .is_some_and(|r| r.contains("re: hello from alice"))
    })
    .await;

    // tool calls carry ids and their results come back paired
    let evs = events(&d, &alice).await;
    let call = evs
        .iter()
        .find(|e| e["kind"] == "tool_use")
        .expect("tool_use event");
    let id = call["payload"]["id"].as_str().expect("tool call id");
    assert!(
        evs.iter()
            .any(|e| e["kind"] == "tool_result" && e["payload"]["id"] == id),
        "no tool_result for {id}"
    );

    // memory through MCP
    http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{bob}/messages"),
        Some(json!({"body": "remember color: blue"})),
    )
    .await;
    http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{bob}/messages"),
        Some(json!({"body": "recall color"})),
    )
    .await;
    wait_event(&d, &bob, |e| {
        e["kind"] == "assistant_text"
            && e["payload"]["text"]
                .as_str()
                .is_some_and(|t| t.contains("blue"))
    })
    .await;

    // ... and visible through the API
    let (_, mem) = http(
        &d.ui,
        "GET",
        &format!("/v1/agents/{bob}/memory?q=color"),
        None,
    )
    .await;
    assert_eq!(mem[0]["content"], "blue", "{mem}");

    // a routine run reaches the runner as a message from routine:<name>
    let (status, r) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{alice}/routines"),
        Some(json!({"name": "standup", "schedule": "0 9 * * 1-5", "prompt": "status report"})),
    )
    .await;
    assert_eq!(status, 201, "{r}");
    let (status, _) = http(
        &d.ui,
        "POST",
        &format!("/v1/routines/{}/run", r["id"]),
        None,
    )
    .await;
    assert_eq!(status, 202);
    wait_event(&d, &alice, |e| {
        e["kind"] == "turn_done"
            && e["payload"]["result"]
                .as_str()
                .is_some_and(|r| r.contains("status report") && r.contains("routine:standup"))
    })
    .await;

    // every message got delivered exactly once
    for id in [&alice, &bob] {
        let (_, msgs) = http(&d.ui, "GET", &format!("/v1/agents/{id}/messages"), None).await;
        for m in msgs
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["recipient"] == id.as_str())
        {
            assert_eq!(m["status"], "delivered", "{m}");
        }
    }

    // the agent's computer: exec inside it
    let (status, out) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{alice}/exec"),
        Some(json!({"cmd": ["sh", "-c", "echo hi > note.txt && cat note.txt"]})),
    )
    .await;
    assert_eq!(status, 200, "{out}");
    assert_eq!(
        (out["exit_code"].as_i64(), out["stdout"].as_str()),
        (Some(0), Some("hi\n"))
    );

    // sleep / auto-wake: runner disconnects, a new message wakes it
    let (status, _) = http(&d.ui, "POST", &format!("/v1/agents/{bob}/stop"), None).await;
    assert_eq!(status, 204);
    eventually("bob disconnected", WAIT, || async {
        let (_, a) = http(&d.ui, "GET", &format!("/v1/agents/{bob}"), None).await;
        (a["connected"] == false).then_some(())
    })
    .await;
    http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{bob}/messages"),
        Some(json!({"body": "queued while asleep"})),
    )
    .await;
    wait_event(&d, &bob, |e| {
        e["kind"] == "turn_done"
            && e["payload"]["result"]
                .as_str()
                .is_some_and(|r| r.contains("queued while asleep"))
    })
    .await;

    // question card: the mock asks via ask_user, the operator answers, the
    // agent gets the answers as a user message
    http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{alice}/messages"),
        Some(json!({"body": "ask Ship it?: yes | no; Notes"})),
    )
    .await;
    let q = wait_event(&d, &alice, |e| e["kind"] == "question").await;
    let (status, body) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{alice}/questions/{}/answer", q["id"]),
        Some(json!({"answers": [
            {"question_id": "q1", "values": ["yes"]},
            {"question_id": "q2", "custom": "after lunch"}
        ]})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    wait_event(&d, &alice, |e| {
        e["kind"] == "turn_done"
            && e["payload"]["result"]
                .as_str()
                .is_some_and(|r| r.contains("Ship it? yes") && r.contains("Notes after lunch"))
    })
    .await;

    for id in [&alice, &bob] {
        let (status, _) = http(&d.ui, "DELETE", &format!("/v1/agents/{id}"), None).await;
        assert_eq!(status, 204);
    }
}

fn runner_entry() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runner/dist/index.js");
    std::fs::canonicalize(&p)
        .unwrap_or_else(|_| {
            panic!(
                "{} missing: run `npm --prefix runner run build`",
                p.display()
            )
        })
        .display()
        .to_string()
}

#[tokio::test]
#[ignore = "needs Node.js and a built runner (npm --prefix runner run build)"]
async fn local_process_backend_two_agents() {
    let tmp = tempfile::tempdir().unwrap();
    let backend: Arc<dyn SandboxBackend> = Arc::new(
        LocalProcessBackend::new(LocalProcessConfig {
            root: tmp.path().to_path_buf(),
            runner_cmd: vec!["node".into(), runner_entry()],
        })
        .unwrap(),
    );
    two_agents_talk(start(backend, None, "127.0.0.1:0").await).await;
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
#[ignore = "needs Docker and the ferrobot-runner:dev image"]
async fn docker_backend_two_agents() {
    use ferrobot_sandbox::{DockerBackend, DockerConfig};
    let backend = DockerBackend::connect(DockerConfig {
        name_prefix: format!("ferrobot-e2e{}-", std::process::id()),
        ..DockerConfig::default()
    })
    .unwrap();
    backend.ping().await.expect("docker reachable");
    // Containers reach the host through host.docker.internal; bind all interfaces.
    let d = start(
        Arc::new(backend),
        Some("http://host.docker.internal:{port}".into()),
        "0.0.0.0:0",
    )
    .await;
    two_agents_talk(d).await;
}
