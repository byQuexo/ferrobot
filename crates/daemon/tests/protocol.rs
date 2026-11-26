//! Daemon <-> runner protocol tests with a fake sandbox backend and a fake
//! runner speaking the WebSocket protocol directly. No Node, no Docker.

#![allow(
    clippy::too_many_lines,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test scenarios read best as one story"
)]

mod common;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{Ws, events, eventually, http, next_frame, send, start};
use ferrobot_core::protocol::{DaemonFrame, RunnerFrame, TurnEvent, TurnOutcome};
use ferrobot_sandbox::{
    ExecOutput, ExecRequest, SandboxBackend, SandboxError, SandboxHandle, SandboxPath, SandboxSpec,
    SandboxState, SnapshotRef,
};
use serde_json::json;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Records specs instead of starting anything.
#[derive(Debug, Default)]
struct FakeBackend {
    envs: Mutex<HashMap<String, BTreeMap<String, String>>>,
}

#[async_trait]
impl SandboxBackend for FakeBackend {
    fn kind(&self) -> &'static str {
        "fake"
    }
    async fn create(&self, spec: &SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        self.envs
            .lock()
            .unwrap()
            .insert(spec.name.clone(), spec.env.clone());
        Ok(SandboxHandle {
            backend: "fake".into(),
            id: spec.name.clone(),
        })
    }
    async fn exec(&self, _: &SandboxHandle, req: &ExecRequest) -> Result<ExecOutput, SandboxError> {
        Ok(ExecOutput {
            exit_code: 0,
            stdout: req.cmd.join(" "),
            stderr: String::new(),
            truncated: false,
        })
    }
    async fn read_file(&self, _: &SandboxHandle, p: &SandboxPath) -> Result<Vec<u8>, SandboxError> {
        Err(SandboxError::NotFound(p.to_string()))
    }
    async fn write_file(
        &self,
        _: &SandboxHandle,
        _: &SandboxPath,
        _: &[u8],
    ) -> Result<(), SandboxError> {
        Ok(())
    }
    async fn snapshot(&self, _: &SandboxHandle, label: &str) -> Result<SnapshotRef, SandboxError> {
        Ok(SnapshotRef {
            id: label.into(),
            scope: "fake".into(),
        })
    }
    async fn wake(&self, _: &SandboxHandle) -> Result<(), SandboxError> {
        Ok(())
    }
    async fn stop(&self, _: &SandboxHandle) -> Result<(), SandboxError> {
        Ok(())
    }
    async fn destroy(&self, _: &SandboxHandle, _: bool) -> Result<(), SandboxError> {
        Ok(())
    }
    async fn state(&self, _: &SandboxHandle) -> Result<SandboxState, SandboxError> {
        Ok(SandboxState::Running)
    }
}

async fn connect_runner(agents: &str, token: &str) -> Ws {
    let mut req = format!("ws://{agents}/runner/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .expect("ws connect");
    let hello = RunnerFrame::Hello {
        protocol_version: ferrobot_core::PROTOCOL_VERSION,
        runner_version: "test".into(),
        driver: "fake".into(),
        session_id: Some("sess-1".into()),
        steering: false,
    };
    send(&mut ws, &hello).await;
    match next_frame(&mut ws).await {
        DaemonFrame::Welcome { agent_name, .. } => assert_ne!(agent_name.as_str(), ""),
        other => panic!("expected welcome, got {other:?}"),
    }
    ws
}

#[tokio::test]
async fn user_message_turn_roundtrip_and_redelivery() {
    let backend = Arc::new(FakeBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;

    let (status, alice) = http(
        &d.ui,
        "POST",
        "/v1/agents",
        Some(json!({"name": "alice", "instructions": "lead"})),
    )
    .await;
    assert_eq!(status, 201, "{alice}");
    assert_eq!(alice["status"], "running");
    let alice_id = alice["id"].as_str().unwrap().to_owned();
    let (status, _) = http(&d.ui, "POST", "/v1/agents", Some(json!({"name": "alice"}))).await;
    assert_eq!(status, 409);
    let (status, _) = http(
        &d.ui,
        "POST",
        "/v1/agents",
        Some(json!({"name": "Bad Name"})),
    )
    .await;
    assert_eq!(status, 400);

    let env = backend.envs.lock().unwrap()["alice"].clone();
    let token = env["FERROBOT_AGENT_TOKEN"].clone();
    assert_eq!(env["FERROBOT_AGENT_NAME"], "alice");
    assert_eq!(token.len(), 64);

    // Unknown tokens are rejected at the upgrade.
    let mut bad = format!("ws://{}/runner/ws", d.agents)
        .into_client_request()
        .unwrap();
    bad.headers_mut()
        .insert("authorization", "Bearer nope".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(bad).await.is_err());

    // A message sent while the runner is offline waits in the queue.
    let (status, _) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{alice_id}/messages"),
        Some(json!({"body": "hello"})),
    )
    .await;
    assert_eq!(status, 202);

    let mut ws = connect_runner(&d.agents, &token).await;
    let DaemonFrame::Turn {
        turn_id,
        body,
        sender_name,
        ..
    } = next_frame(&mut ws).await
    else {
        panic!("expected turn");
    };
    assert_eq!((body.as_str(), sender_name.as_str()), ("hello", "user"));

    // Disconnect mid-turn: the message must be redelivered on reconnect.
    drop(ws);
    let mut ws = connect_runner(&d.agents, &token).await;
    let DaemonFrame::Turn {
        turn_id: turn2,
        body,
        ..
    } = next_frame(&mut ws).await
    else {
        panic!("expected redelivered turn");
    };
    assert_ne!(turn_id, turn2);
    assert_eq!(body, "hello");

    send(
        &mut ws,
        &RunnerFrame::Event {
            turn_id: turn2.clone(),
            event: TurnEvent::AssistantText { text: "hi!".into() },
        },
    )
    .await;
    send(
        &mut ws,
        &RunnerFrame::TurnDone {
            turn_id: turn2.clone(),
            outcome: TurnOutcome::Completed,
            session_id: Some("sess-2".into()),
            result: Some("hi!".into()),
            error: None,
            steered: vec![],
        },
    )
    .await;

    let kinds = eventually("turn_done event", Duration::from_secs(5), || async {
        let kinds: Vec<String> = events(&d, &alice_id)
            .await
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_owned())
            .collect();
        kinds.contains(&"turn_done".to_owned()).then_some(kinds)
    })
    .await;
    assert!(kinds.contains(&"assistant_text".to_owned()), "{kinds:?}");

    let (_, msgs) = http(
        &d.ui,
        "GET",
        &format!("/v1/agents/{alice_id}/messages"),
        None,
    )
    .await;
    eventually("delivered", Duration::from_secs(5), || async {
        let (_, msgs) = http(
            &d.ui,
            "GET",
            &format!("/v1/agents/{alice_id}/messages"),
            None,
        )
        .await;
        (msgs[0]["status"] == "delivered").then_some(())
    })
    .await;
    assert_eq!(msgs[0]["attempts"], 2);
    let (_, agent) = http(&d.ui, "GET", &format!("/v1/agents/{alice_id}"), None).await;
    assert_eq!(agent["session_id"], "sess-2");
    assert_eq!(agent["connected"], true);
}

#[tokio::test]
async fn interrupt_reaches_runner() {
    let backend = Arc::new(FakeBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let (_, bob) = http(&d.ui, "POST", "/v1/agents", Some(json!({"name": "bob"}))).await;
    let bob_id = bob["id"].as_str().unwrap().to_owned();
    let token = backend.envs.lock().unwrap()["bob"]["FERROBOT_AGENT_TOKEN"].clone();
    let mut ws = connect_runner(&d.agents, &token).await;

    http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{bob_id}/messages"),
        Some(json!({"body": "long task"})),
    )
    .await;
    let DaemonFrame::Turn { turn_id, .. } = next_frame(&mut ws).await else {
        panic!("turn")
    };

    let (status, r) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{bob_id}/interrupt"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(r["turn_id"], turn_id.as_str());
    match next_frame(&mut ws).await {
        DaemonFrame::Interrupt { turn_id: t } => assert_eq!(t, turn_id),
        other => panic!("expected interrupt, got {other:?}"),
    }
    send(
        &mut ws,
        &RunnerFrame::TurnDone {
            turn_id,
            outcome: TurnOutcome::Interrupted,
            session_id: None,
            result: None,
            error: None,
            steered: vec![],
        },
    )
    .await;
    // Interrupted messages are settled, not retried.
    eventually("settled", Duration::from_secs(5), || async {
        let (_, msgs) = http(&d.ui, "GET", &format!("/v1/agents/{bob_id}/messages"), None).await;
        (msgs[0]["status"] == "delivered").then_some(())
    })
    .await;

    let (status, out) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{bob_id}/exec"),
        Some(json!({"cmd": ["echo", "hi"]})),
    )
    .await;
    assert_eq!((status, out["stdout"].as_str()), (200, Some("echo hi")));
    let (status, _) = http(&d.ui, "DELETE", &format!("/v1/agents/{bob_id}"), None).await;
    assert_eq!(status, 204);
    let (status, _) = http(&d.ui, "GET", &format!("/v1/agents/{bob_id}"), None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn new_session_marks_only_the_next_turn() {
    let backend = Arc::new(FakeBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let (_, cy) = http(&d.ui, "POST", "/v1/agents", Some(json!({"name": "cy"}))).await;
    let cy_id = cy["id"].as_str().unwrap().to_owned();
    let token = backend.envs.lock().unwrap()["cy"]["FERROBOT_AGENT_TOKEN"].clone();
    let unknown = "/v1/agents/00000000-0000-4000-8000-000000000000/new-session";
    assert_eq!(http(&d.ui, "POST", unknown, None).await.0, 404);
    let reset = format!("/v1/agents/{cy_id}/new-session");
    assert_eq!(http(&d.ui, "POST", &reset, None).await.0, 204);

    let mut ws = connect_runner(&d.agents, &token).await;
    for (body, fresh) in [("one", true), ("two", false)] {
        http(
            &d.ui,
            "POST",
            &format!("/v1/agents/{cy_id}/messages"),
            Some(json!({ "body": body })),
        )
        .await;
        let DaemonFrame::Turn {
            turn_id,
            new_session,
            ..
        } = next_frame(&mut ws).await
        else {
            panic!("expected a turn");
        };
        assert_eq!(new_session, fresh, "{body}");
        send(
            &mut ws,
            &RunnerFrame::TurnDone {
                turn_id,
                outcome: TurnOutcome::Completed,
                session_id: Some(format!("s-{body}")),
                result: None,
                error: None,
                steered: vec![],
            },
        )
        .await;
    }
}

#[test]
fn openapi_conformance_check_rejects_wrong_shapes() {
    use common::assert_conforms;
    let bad_body = std::panic::catch_unwind(|| {
        assert_conforms("GET", "/v1/agents", 200, &json!([{"id": 1}]));
    });
    assert!(bad_body.is_err(), "malformed AgentView must be rejected");
    let bad_status = std::panic::catch_unwind(|| {
        assert_conforms("GET", "/v1/health", 418, &json!({}));
    });
    assert!(bad_status.is_err(), "undocumented status must be rejected");
    assert_conforms(
        "GET",
        "/v1/agents/abc",
        404,
        &json!({"error": "not_found", "message": "agent abc not found"}),
    );
}
