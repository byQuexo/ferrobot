//! Phase-1 features against a stateful fake backend: profiles and avatars,
//! memory, routines and the scheduler, idle sleep and auto-wake, snapshots,
//! and the model proxy. No Node, no Docker.

#![allow(clippy::too_many_lines)]

mod common;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use common::{Daemon, events, eventually, http, http_raw, start, start_on, start_with};
use ferrobot_core::protocol::{DaemonFrame, RunnerFrame};
use ferrobot_core::{AgentId, Sender};
use ferrobot_mcp::MailboxNotifier;
use ferrobot_sandbox::{
    ExecOutput, ExecRequest, SandboxBackend, SandboxError, SandboxHandle, SandboxPath, SandboxSpec,
    SandboxState, SleepLevel, SnapshotRef,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Tracks container state like a real engine would.
#[derive(Debug, Default)]
struct StatefulBackend {
    envs: Mutex<HashMap<String, BTreeMap<String, String>>>,
    states: Mutex<HashMap<String, SandboxState>>,
    log: Mutex<Vec<String>>,
}

impl StatefulBackend {
    fn set(&self, id: &str, s: SandboxState) {
        self.states.lock().unwrap().insert(id.into(), s);
    }
    fn log(&self, what: String) {
        self.log.lock().unwrap().push(what);
    }
    fn calls(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
    fn token(&self, name: &str) -> String {
        self.envs.lock().unwrap()[name]["FERROBOT_AGENT_TOKEN"].clone()
    }
}

#[async_trait]
impl SandboxBackend for StatefulBackend {
    fn kind(&self) -> &'static str {
        "fake"
    }
    async fn create(&self, spec: &SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        self.envs
            .lock()
            .unwrap()
            .insert(spec.name.clone(), spec.env.clone());
        self.set(&spec.name, SandboxState::Running);
        self.log(format!("create {}", spec.name));
        Ok(SandboxHandle {
            backend: "fake".into(),
            id: spec.name.clone(),
        })
    }
    async fn exec(&self, _: &SandboxHandle, _: &ExecRequest) -> Result<ExecOutput, SandboxError> {
        Ok(ExecOutput {
            exit_code: 0,
            stdout: String::new(),
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
    async fn snapshot(&self, h: &SandboxHandle, label: &str) -> Result<SnapshotRef, SandboxError> {
        self.log(format!("snapshot {} {label}", h.id));
        Ok(SnapshotRef {
            id: format!("snap-{label}"),
            scope: "fake disk".into(),
        })
    }
    async fn restore(
        &self,
        h: &SandboxHandle,
        snapshot: &str,
        spec: &SandboxSpec,
    ) -> Result<SandboxHandle, SandboxError> {
        self.log(format!("restore {} {snapshot}", h.id));
        self.create(spec).await
    }
    async fn delete_snapshot(&self, snapshot: &str) -> Result<(), SandboxError> {
        self.log(format!("delete_snapshot {snapshot}"));
        Ok(())
    }
    async fn wake(&self, h: &SandboxHandle) -> Result<(), SandboxError> {
        self.set(&h.id, SandboxState::Running);
        self.log(format!("wake {}", h.id));
        Ok(())
    }
    async fn sleep(
        &self,
        h: &SandboxHandle,
        level: SleepLevel,
    ) -> Result<SleepLevel, SandboxError> {
        if level == SleepLevel::Cold {
            self.stop(h).await?;
        } else {
            self.set(&h.id, SandboxState::Paused);
            self.log(format!("pause {}", h.id));
        }
        Ok(level)
    }
    async fn stop(&self, h: &SandboxHandle) -> Result<(), SandboxError> {
        self.set(&h.id, SandboxState::Stopped);
        self.log(format!("stop {}", h.id));
        Ok(())
    }
    async fn destroy(&self, h: &SandboxHandle, purge: bool) -> Result<(), SandboxError> {
        self.states.lock().unwrap().remove(&h.id);
        self.log(format!("destroy {} purge={purge}", h.id));
        Ok(())
    }
    async fn state(&self, h: &SandboxHandle) -> Result<SandboxState, SandboxError> {
        Ok(self
            .states
            .lock()
            .unwrap()
            .get(&h.id)
            .copied()
            .unwrap_or(SandboxState::Missing))
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_runner(agents: &str, token: &str) -> Ws {
    let mut req = format!("ws://{agents}/runner/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let hello = RunnerFrame::Hello {
        protocol_version: ferrobot_core::PROTOCOL_VERSION,
        runner_version: "test".into(),
        driver: "fake".into(),
        session_id: None,
    };
    ws.send(Message::text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut ws).await,
        DaemonFrame::Welcome { .. }
    ));
    ws
}

async fn next_frame(ws: &mut Ws) -> DaemonFrame {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("frame in time")
            .expect("stream open")
            .expect("ws ok");
        if let Message::Text(t) = msg {
            return serde_json::from_str(&t).expect("daemon frame");
        }
    }
}

async fn create(d: &Daemon, body: Value) -> Value {
    let (status, agent) = http(&d.ui, "POST", "/v1/agents", Some(body)).await;
    assert_eq!(status, 201, "{agent}");
    agent
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR-not-a-real-image";

#[tokio::test]
async fn profile_avatar_unread_and_memory() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let a = create(
        &d,
        json!({"name": "ada", "description": "  Research lead ", "instructions": "be brief"}),
    )
    .await;
    let id = a["id"].as_str().unwrap().to_owned();
    assert_eq!(a["description"], "Research lead");
    assert_eq!(a["avatar_url"], Value::Null);
    assert_eq!(a["unread"], 0);

    // Instructions edits reach a connected runner as a configure frame.
    let mut ws = connect_runner(&d.agents, &backend.token("ada")).await;
    let (status, a) = http(
        &d.ui,
        "PATCH",
        &format!("/v1/agents/{id}"),
        Some(json!({"instructions": "be thorough"})),
    )
    .await;
    assert_eq!(status, 200, "{a}");
    assert_eq!(a["instructions"], "be thorough");
    assert_eq!(a["description"], "Research lead", "untouched");
    match next_frame(&mut ws).await {
        DaemonFrame::Configure {
            agent_name,
            instructions,
        } => {
            assert_eq!(
                (agent_name.as_str(), instructions.as_str()),
                ("ada", "be thorough")
            );
        }
        other => panic!("expected configure, got {other:?}"),
    }

    // Renaming keeps the id and sandbox and tells the runner; names stay unique.
    let (status, a) = http(
        &d.ui,
        "PATCH",
        &format!("/v1/agents/{id}"),
        Some(json!({"name": "ada-lead"})),
    )
    .await;
    assert_eq!(status, 200, "{a}");
    assert_eq!(
        (a["name"].as_str(), a["sandbox_name"].as_str()),
        (Some("ada-lead"), Some("ada"))
    );
    assert_eq!(a["id"], id.as_str());
    match next_frame(&mut ws).await {
        DaemonFrame::Configure { agent_name, .. } => assert_eq!(agent_name.as_str(), "ada-lead"),
        other => panic!("expected configure, got {other:?}"),
    }
    create(&d, json!({"name": "taken"})).await;
    for (name, want) in [("taken", 409), ("Bad Name", 400), ("ada", 200)] {
        let (status, body) = http(
            &d.ui,
            "PATCH",
            &format!("/v1/agents/{id}"),
            Some(json!({ "name": name })),
        )
        .await;
        assert_eq!(status, want, "{name}: {body}");
    }
    let _ = next_frame(&mut ws).await; // configure for the rename back
    let (status, _) = http(
        &d.ui,
        "PATCH",
        &format!("/v1/agents/{id}"),
        Some(json!({"description": "x".repeat(300)})),
    )
    .await;
    assert_eq!(status, 400);

    // Avatar: type must match the bytes, then it is served back.
    let path = format!("/v1/agents/{id}/avatar");
    let (status, _, _) =
        http_raw(&d.ui, "PUT", &path, &[("Content-Type", "image/jpeg")], PNG).await;
    assert_eq!(status, 400);
    let (status, _, _) = http_raw(
        &d.ui,
        "PUT",
        &path,
        &[("Content-Type", "image/svg+xml")],
        b"<svg/>",
    )
    .await;
    assert_eq!(status, 400);
    let (status, _, body) =
        http_raw(&d.ui, "PUT", &path, &[("Content-Type", "image/png")], PNG).await;
    assert_eq!(status, 200);
    let view: Value = serde_json::from_slice(&body).unwrap();
    common::assert_conforms("PUT", &path, 200, &view);
    let url = view["avatar_url"].as_str().unwrap().to_owned();
    assert!(url.starts_with(&path), "{url}");
    let (status, headers, body) = http_raw(&d.ui, "GET", &url, &[], b"").await;
    assert_eq!(status, 200);
    assert_eq!(body, PNG);
    assert!(headers.contains(&("content-type".into(), "image/png".into())));
    let (status, view) = http(&d.ui, "DELETE", &path, None).await;
    assert_eq!(status, 200);
    assert_eq!(view["avatar_url"], Value::Null);
    let (status, _, _) = http_raw(&d.ui, "GET", &path, &[], b"").await;
    assert_eq!(status, 404);

    // Unread counts finished turns until the chat is marked read.
    let (status, _) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{id}/messages"),
        Some(json!({"body": "hello"})),
    )
    .await;
    assert_eq!(status, 202);
    let turn_id = match next_frame(&mut ws).await {
        DaemonFrame::Turn { turn_id, .. } => turn_id,
        other => panic!("expected turn, got {other:?}"),
    };
    // Streaming deltas go to UI clients but are not persisted.
    let mut ui = d.state.ui.subscribe();
    let delta = RunnerFrame::Event {
        turn_id: turn_id.clone(),
        event: ferrobot_core::protocol::TurnEvent::AssistantDelta { text: "hi".into() },
    };
    ws.send(Message::text(serde_json::to_string(&delta).unwrap()))
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ferrobot_daemon::UiEvent::Delta {
                text, turn_id: t, ..
            } = ui.recv().await.unwrap()
            {
                break (t, text);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(got, (turn_id.clone(), "hi".to_owned()));
    let done = RunnerFrame::TurnDone {
        turn_id,
        outcome: ferrobot_core::protocol::TurnOutcome::Completed,
        session_id: None,
        result: Some("hi".into()),
        error: None,
    };
    ws.send(Message::text(serde_json::to_string(&done).unwrap()))
        .await
        .unwrap();
    eventually("unread", Duration::from_secs(5), || async {
        let (_, a) = http(&d.ui, "GET", &format!("/v1/agents/{id}"), None).await;
        (a["unread"] == 1).then_some(())
    })
    .await;
    let (status, _) = http(&d.ui, "POST", &format!("/v1/agents/{id}/read"), None).await;
    assert_eq!(status, 204);
    let (_, a) = http(&d.ui, "GET", &format!("/v1/agents/{id}"), None).await;
    assert_eq!(a["unread"], 0);
    assert_eq!(a["presence"], "awake");
    let kinds: Vec<Value> = events(&d, &id)
        .await
        .iter()
        .map(|e| e["kind"].clone())
        .collect();
    assert!(!kinds.contains(&json!("assistant_delta")), "{kinds:?}");

    // Memory CRUD + search.
    let mem = format!("/v1/agents/{id}/memory");
    for (key, content) in [("db", "Postgres on port 5432"), ("lang", "Rust first")] {
        let (status, _) = http(
            &d.ui,
            "POST",
            &mem,
            Some(json!({"key": key, "content": content})),
        )
        .await;
        assert_eq!(status, 204);
    }
    let (status, _) = http(
        &d.ui,
        "POST",
        &mem,
        Some(json!({"key": "", "content": "x"})),
    )
    .await;
    assert_eq!(status, 400);
    let (_, all) = http(&d.ui, "GET", &mem, None).await;
    assert_eq!(all.as_array().unwrap().len(), 2);
    let (_, hits) = http(&d.ui, "GET", &format!("{mem}?q=postgres"), None).await;
    assert_eq!(hits[0]["key"], "db", "{hits}");
    assert_eq!(hits.as_array().unwrap().len(), 1);
    let (status, _) = http(&d.ui, "DELETE", &format!("{mem}/db"), None).await;
    assert_eq!(status, 204);
    let (status, _) = http(&d.ui, "DELETE", &format!("{mem}/db"), None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn routines_crud_run_now_and_schedule() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let id = create(&d, json!({"name": "rex"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let list = format!("/v1/agents/{id}/routines");
    for bad in [
        json!({"name": "x", "schedule": "61 * * * *", "prompt": "p"}),
        json!({"name": "x", "schedule": "0 9 * * *", "prompt": "p", "timezone": "Mars/Base"}),
        json!({"name": "x", "schedule": "0 9 * * *", "prompt": " "}),
    ] {
        let (status, _) = http(&d.ui, "POST", &list, Some(bad)).await;
        assert_eq!(status, 400);
    }
    let (status, r) = http(
        &d.ui,
        "POST",
        &list,
        Some(json!({"name": "digest", "schedule": "0 9 * * 1-5",
                    "prompt": "Summarize the news", "timezone": "Europe/Berlin"})),
    )
    .await;
    assert_eq!(status, 201, "{r}");
    assert!(r["next_run_at"].is_string());
    assert_ne!(r["schedule_text"], "");
    let rid = r["id"].as_i64().unwrap();
    let (status, _) = http(
        &d.ui,
        "POST",
        &list,
        Some(json!({"name": "digest", "schedule": "@daily", "prompt": "p"})),
    )
    .await;
    assert_eq!(status, 409);

    // Disable / enable.
    let (_, r) = http(
        &d.ui,
        "PATCH",
        &format!("/v1/routines/{rid}"),
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(r["enabled"], false);
    assert_eq!(r["next_run_at"], Value::Null);
    let (_, r) = http(
        &d.ui,
        "PATCH",
        &format!("/v1/routines/{rid}"),
        Some(json!({"enabled": true, "schedule": "@hourly"})),
    )
    .await;
    assert_eq!(r["schedule"], "@hourly");
    let next = r["next_run_at"].clone();

    // Run now: queues the prompt from `routine:<id>`, keeps the schedule.
    let (status, msg) = http(&d.ui, "POST", &format!("/v1/routines/{rid}/run"), None).await;
    assert_eq!(status, 202, "{msg}");
    assert_eq!(msg["sender"], json!({"kind": "routine", "id": rid}));
    assert_eq!(msg["body"], "Summarize the news");
    let (_, runs) = http(&d.ui, "GET", &format!("/v1/routines/{rid}/runs"), None).await;
    assert_eq!(runs[0]["trigger"], "manual");
    let (_, rs) = http(&d.ui, "GET", &list, None).await;
    assert_eq!(rs[0]["next_run_at"], next);
    assert!(rs[0]["last_run_at"].is_string());

    // The scheduler fires due routines and moves them to the next slot.
    let mut routine = d.state.store.get_routine(rid).await.unwrap();
    routine.next_run_at = Some(time::OffsetDateTime::now_utc() - time::Duration::minutes(1));
    d.state.store.update_routine(routine).await.unwrap();
    tokio::spawn(ferrobot_daemon::scheduler::run(d.state.clone()));
    let runs = eventually("scheduled run", Duration::from_secs(5), || async {
        let (_, runs) = http(&d.ui, "GET", &format!("/v1/routines/{rid}/runs"), None).await;
        (runs.as_array().unwrap().len() == 2).then_some(runs)
    })
    .await;
    assert_eq!(runs[0]["trigger"], "schedule");
    let after = d.state.store.get_routine(rid).await.unwrap();
    assert!(after.next_run_at.unwrap() > time::OffsetDateTime::now_utc());

    // The routine prompt is delivered to the agent like any message.
    let mut ws = connect_runner(&d.agents, &backend.token("rex")).await;
    match next_frame(&mut ws).await {
        DaemonFrame::Turn {
            sender_name, body, ..
        } => {
            assert_eq!(sender_name, "routine:digest");
            assert_eq!(body, "Summarize the news");
        }
        other => panic!("expected turn, got {other:?}"),
    }

    let (status, _) = http(&d.ui, "DELETE", &format!("/v1/routines/{rid}"), None).await;
    assert_eq!(status, 204);
    let (status, _) = http(&d.ui, "GET", &format!("/v1/routines/{rid}/runs"), None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn idle_sleep_auto_wake_and_crash_recovery() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start_with(backend.clone(), None, "127.0.0.1:0", |s| {
        s.idle_timeout = Some(Duration::from_millis(300));
    })
    .await;
    let id = create(&d, json!({"name": "sam"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ws = connect_runner(&d.agents, &backend.token("sam")).await;

    // Not idle long enough yet.
    ferrobot_daemon::supervisor::check_all(&d.state).await;
    let (_, a) = http(&d.ui, "GET", &format!("/v1/agents/{id}"), None).await;
    assert_eq!(a["status"], "running");

    tokio::time::sleep(Duration::from_millis(400)).await;
    ferrobot_daemon::supervisor::check_all(&d.state).await;
    let (_, a) = http(&d.ui, "GET", &format!("/v1/agents/{id}"), None).await;
    assert_eq!(a["status"], "stopped");
    assert_eq!(a["presence"], "sleeping");
    assert!(
        backend.calls().contains(&"stop sam".to_owned()),
        "{:?}",
        backend.calls()
    );
    let (_, info) = http(&d.ui, "GET", &format!("/v1/agents/{id}/sandbox"), None).await;
    assert_eq!(info["state"], "stopped");
    drop(ws);

    // A message wakes it again.
    let (status, _) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{id}/messages"),
        Some(json!({"body": "wake up"})),
    )
    .await;
    assert_eq!(status, 202);
    eventually("auto-wake", Duration::from_secs(5), || async {
        let (_, a) = http(&d.ui, "GET", &format!("/v1/agents/{id}"), None).await;
        (a["status"] == "running").then_some(())
    })
    .await;
    let kinds: Vec<Value> = events(&d, &id)
        .await
        .iter()
        .map(|e| e["kind"].clone())
        .collect();
    assert!(kinds.contains(&json!("sleeping")), "{kinds:?}");
    assert!(kinds.contains(&json!("awake")), "{kinds:?}");

    // Crash recovery: the container died behind the daemon's back.
    let wakes = |calls: Vec<String>| calls.iter().filter(|c| c.starts_with("wake")).count();
    let before = wakes(backend.calls());
    let handle = d
        .state
        .store
        .get_agent(id.parse().unwrap())
        .await
        .unwrap()
        .sandbox_id
        .unwrap();
    backend.set(&handle, SandboxState::Stopped);
    ferrobot_daemon::supervisor::check_all(&d.state).await;
    assert_eq!(wakes(backend.calls()), before + 1, "{:?}", backend.calls());
    let kinds: Vec<Value> = events(&d, &id)
        .await
        .iter()
        .map(|e| e["kind"].clone())
        .collect();
    assert!(kinds.contains(&json!("sandbox_restarted")), "{kinds:?}");

    // Manual stop / wake.
    let (status, _) = http(&d.ui, "POST", &format!("/v1/agents/{id}/stop"), None).await;
    assert_eq!(status, 204);
    let (status, _) = http(&d.ui, "POST", &format!("/v1/agents/{id}/wake"), None).await;
    assert_eq!(status, 204);
    let (_, a) = http(&d.ui, "GET", &format!("/v1/agents/{id}"), None).await;
    assert_eq!(a["status"], "running");
}

#[tokio::test]
async fn snapshots_create_restore_delete() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let id = create(&d, json!({"name": "sid"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let old_token = backend.token("sid");
    let snaps = format!("/v1/agents/{id}/snapshots");
    let (status, s) = http(
        &d.ui,
        "POST",
        &snaps,
        Some(json!({"label": "before-upgrade"})),
    )
    .await;
    assert_eq!(status, 201, "{s}");
    assert_eq!(s["snapshot_ref"], "snap-before-upgrade");
    let sid = s["id"].as_i64().unwrap();
    let (_, list) = http(&d.ui, "GET", &snaps, None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    let (status, _) = http(&d.ui, "POST", &format!("{snaps}/{sid}/restore"), None).await;
    assert_eq!(status, 204);
    assert!(
        backend
            .calls()
            .iter()
            .any(|c| c.starts_with("restore") && c.ends_with("snap-before-upgrade")),
        "{:?}",
        backend.calls()
    );
    assert_ne!(
        backend.token("sid"),
        old_token,
        "restored sandbox gets a fresh token"
    );
    let (status, _) = http(&d.ui, "POST", &format!("{snaps}/999/restore"), None).await;
    assert_eq!(status, 404);

    let (status, _) = http(&d.ui, "DELETE", &format!("{snaps}/{sid}"), None).await;
    assert_eq!(status, 204);
    assert!(
        backend
            .calls()
            .contains(&"delete_snapshot snap-before-upgrade".to_owned())
    );
    let (_, list) = http(&d.ui, "GET", &snaps, None).await;
    assert_eq!(list, json!([]));

    // Deleting an agent purges its disk and snapshots unless `keep_data`.
    let (status, _) = http(&d.ui, "POST", &snaps, Some(json!({"label": "last"}))).await;
    assert_eq!(status, 201);
    let (status, _) = http(&d.ui, "DELETE", &format!("/v1/agents/{id}"), None).await;
    assert_eq!(status, 204);
    let calls = backend.calls();
    assert!(
        calls.contains(&"destroy sid purge=true".to_owned()),
        "{calls:?}"
    );
    assert!(
        calls.contains(&"delete_snapshot snap-last".to_owned()),
        "{calls:?}"
    );

    let keep = create(&d, json!({"name": "keeper"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let keep_snaps = format!("/v1/agents/{keep}/snapshots");
    let (status, _) = http(&d.ui, "POST", &keep_snaps, Some(json!({"label": "kept"}))).await;
    assert_eq!(status, 201);
    let path = format!("/v1/agents/{keep}?keep_data=true");
    let (status, _) = http(&d.ui, "DELETE", &path, None).await;
    assert_eq!(status, 204);
    let calls = backend.calls();
    assert!(
        calls.contains(&"destroy keeper purge=false".to_owned()),
        "{calls:?}"
    );
    assert!(
        !calls.contains(&"delete_snapshot snap-kept".to_owned()),
        "{calls:?}"
    );
}

#[tokio::test]
async fn operator_api_requires_the_token() {
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let get = |path: &'static str, auth: &'static str| {
        let ui = d.ui.clone();
        async move {
            let (status, headers, _) =
                http_raw(&ui, "GET", path, &[("Authorization", auth)], b"").await;
            (status, headers)
        }
    };
    // Health stays open.
    assert_eq!(get("/v1/health", "").await.0, 200);
    let (status, headers) = get("/v1/agents", "").await;
    assert_eq!(status, 401);
    assert!(headers.contains(&("www-authenticate".into(), "Bearer".into())));
    assert_eq!(
        get("/v1/agents", "Bearer wrong-token-0123456789").await.0,
        401
    );
    assert_eq!(
        get("/v1/agents", "Bearer test-operator-token-0123456789")
            .await
            .0,
        200
    );
    // GET (WebSocket, images) may use the query parameter instead.
    let with_query = "/v1/agents?access_token=test-operator-token-0123456789";
    assert_eq!(get(with_query, "").await.0, 200);
    assert_eq!(get("/v1/ws?access_token=nope", "").await.0, 401);
    // ... but other methods may not.
    let (status, _, _) = http_raw(
        &d.ui,
        "POST",
        "/v1/agents?access_token=test-operator-token-0123456789",
        &[("Authorization", ""), ("Content-Type", "application/json")],
        br#"{"name":"x"}"#,
    )
    .await;
    assert_eq!(status, 401);
}

/// Fake model API: echoes the key it saw and streams two SSE chunks.
async fn fake_upstream() -> String {
    async fn handler(headers: HeaderMap, body: String) -> axum::response::Response {
        let key = headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("none")
            .to_owned();
        let stream = futures_util::stream::iter([
            Ok::<_, std::io::Error>(format!("event: key\ndata: {key}\n\n")),
            Ok(format!("event: body\ndata: {body}\n\n")),
        ]);
        (
            [("content-type", "text/event-stream")],
            Body::from_stream(stream),
        )
            .into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().route("/v1/messages", axum::routing::post(handler));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn model_proxy_swaps_key_and_streams() {
    let upstream = fake_upstream().await;
    let backend = Arc::new(StatefulBackend::default());
    let d = start_with(backend.clone(), None, "127.0.0.1:0", |s| {
        s.driver = "claude".into();
        s.anthropic_api_key = Some("sk-real-secret".into());
        s.anthropic_upstream = upstream;
    })
    .await;
    create(&d, json!({"name": "pia"})).await;
    let env = backend.envs.lock().unwrap()["pia"].clone();
    let token = env["FERROBOT_AGENT_TOKEN"].clone();
    // The sandbox only ever sees its own token.
    assert_eq!(env["ANTHROPIC_API_KEY"], token);
    assert!(env["ANTHROPIC_BASE_URL"].ends_with("/model"), "{env:?}");
    assert!(!env.values().any(|v| v.contains("sk-real-secret")));

    let body = br#"{"model":"m","stream":true}"#;
    let (status, headers, out) = http_raw(
        &d.agents,
        "POST",
        "/model/v1/messages",
        &[("x-api-key", &token), ("content-type", "application/json")],
        body,
    )
    .await;
    let out = String::from_utf8(out).unwrap();
    assert_eq!(status, 200, "{out}");
    assert!(headers.contains(&("content-type".into(), "text/event-stream".into())));
    assert!(out.contains("data: sk-real-secret"), "{out}");
    assert!(
        out.contains(r#"data: {"model":"m","stream":true}"#),
        "{out}"
    );

    // Bearer form works too; bad tokens and non-API paths do not.
    let bearer = format!("Bearer {token}");
    let (status, _, _) = http_raw(
        &d.agents,
        "POST",
        "/model/v1/messages",
        &[("authorization", &bearer)],
        body,
    )
    .await;
    assert_eq!(status, 200);
    let (status, _, _) = http_raw(
        &d.agents,
        "POST",
        "/model/v1/messages",
        &[("x-api-key", "nope")],
        body,
    )
    .await;
    assert_eq!(status, 401);
    let (status, _, _) = http_raw(
        &d.agents,
        "GET",
        "/model/admin",
        &[("x-api-key", &token)],
        b"",
    )
    .await;
    assert_eq!(status, 404);

    // Without a configured key the proxy tells the (authenticated) agent so.
    let backend2 = Arc::new(StatefulBackend::default());
    let d2 = start(backend2.clone(), None, "127.0.0.1:0").await;
    create(&d2, json!({"name": "pia"})).await;
    let (status, _, out) = http_raw(
        &d2.agents,
        "POST",
        "/model/v1/messages",
        &[("x-api-key", &backend2.token("pia"))],
        body,
    )
    .await;
    assert_eq!(status, 503);
    assert!(String::from_utf8_lossy(&out).contains("ferrobot_proxy_error"));
}

#[tokio::test]
async fn state_survives_a_daemon_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ferrobot.db");
    let backend = Arc::new(StatefulBackend::default());
    let d = start_on(
        ferrobot_store::Store::open(&db).unwrap(),
        backend.clone(),
        None,
        "127.0.0.1:0",
        |_| {},
    )
    .await;
    let awake = create(&d, json!({"name": "amy", "description": "stays up"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let asleep = create(&d, json!({"name": "bob"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{awake}/memory"),
        Some(json!({"key": "k", "content": "remember me"})),
    )
    .await;
    let (status, _) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{awake}/routines"),
        Some(json!({"name": "r", "schedule": "@daily", "prompt": "p", "timezone": "UTC"})),
    )
    .await;
    assert_eq!(status, 201);
    http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{awake}/messages"),
        Some(json!({"body": "queued while down"})),
    )
    .await;
    let (status, _) = http(&d.ui, "POST", &format!("/v1/agents/{asleep}/stop"), None).await;
    assert_eq!(status, 204);
    let amy = d
        .state
        .store
        .get_agent(awake.parse().unwrap())
        .await
        .unwrap();
    let bob = d
        .state
        .store
        .get_agent(asleep.parse().unwrap())
        .await
        .unwrap();
    drop(d);

    // While the daemon is "down", amy's container dies and bob's is started by hand.
    backend.set(amy.sandbox_id.as_deref().unwrap(), SandboxState::Stopped);
    backend.set(bob.sandbox_id.as_deref().unwrap(), SandboxState::Running);

    let d = start_on(
        ferrobot_store::Store::open(&db).unwrap(),
        backend.clone(),
        None,
        "127.0.0.1:0",
        |_| {},
    )
    .await;
    ferrobot_daemon::lifecycle::reconcile(&d.state).await;

    let (_, agents) = http(&d.ui, "GET", "/v1/agents", None).await;
    let names: Vec<&str> = agents
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["amy", "bob"]);
    assert_eq!(agents[0]["description"], "stays up");
    assert_eq!(agents[0]["status"], "running");
    assert_eq!(agents[1]["status"], "stopped");
    let (_, info) = http(&d.ui, "GET", &format!("/v1/agents/{awake}/sandbox"), None).await;
    assert_eq!(info["state"], "running", "dead container was woken");
    let (_, info) = http(&d.ui, "GET", &format!("/v1/agents/{asleep}/sandbox"), None).await;
    assert_eq!(
        info["state"], "stopped",
        "sleeping agent's container put back to sleep"
    );

    let (_, mem) = http(&d.ui, "GET", &format!("/v1/agents/{awake}/memory"), None).await;
    assert_eq!(mem[0]["content"], "remember me");
    let (_, routines) = http(&d.ui, "GET", &format!("/v1/agents/{awake}/routines"), None).await;
    assert_eq!(routines[0]["name"], "r");
    let (_, msgs) = http(&d.ui, "GET", &format!("/v1/agents/{awake}/messages"), None).await;
    assert_eq!(msgs[0]["body"], "queued while down");

    // The queued message is delivered once the runner reconnects.
    let mut ws = connect_runner(&d.agents, &backend.token("amy")).await;
    match next_frame(&mut ws).await {
        DaemonFrame::Turn { body, .. } => assert_eq!(body, "queued while down"),
        other => panic!("expected turn, got {other:?}"),
    }
}

#[tokio::test]
async fn mentions_priorities_and_conversations() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let id_of = |v: Value| v["id"].as_str().unwrap().to_owned();
    let ada = id_of(create(&d, json!({"name": "ada"})).await);
    let bob = id_of(create(&d, json!({"name": "bob"})).await);

    // Mentions are stored by id; unknown names stay text.
    let (status, m) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{ada}/messages"),
        Some(json!({"body": "ask @Bob and @nobody"})),
    )
    .await;
    assert_eq!(status, 202, "{m}");
    assert_eq!(m["body"], format!("ask <@{bob}> and @nobody"));
    // After a rename the model sees the current name.
    let path = format!("/v1/agents/{bob}");
    let (status, _) = http(&d.ui, "PATCH", &path, Some(json!({"name": "robert"}))).await;
    assert_eq!(status, 200);
    let mut ws = connect_runner(&d.agents, &backend.token("ada")).await;
    match next_frame(&mut ws).await {
        DaemonFrame::Turn { body, .. } => assert_eq!(body, "ask @robert and @nobody"),
        other => panic!("expected a turn, got {other:?}"),
    }

    // priority: false queues without waking a sleeping recipient; true wakes it.
    let (status, _) = http(&d.ui, "POST", &format!("/v1/agents/{bob}/stop"), None).await;
    assert_eq!(status, 204);
    let (ada_id, bob_id): (AgentId, AgentId) = (ada.parse().unwrap(), bob.parse().unwrap());
    let send = |body: &str, priority: Option<bool>| {
        let state = d.state.clone();
        let body = body.to_owned();
        async move {
            let m = state
                .store
                .enqueue_message_with_priority(Sender::Agent(ada_id), bob_id, body, None, priority)
                .await
                .unwrap();
            state.message_enqueued(&m);
        }
    };
    send("later", Some(false)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (_, b) = http(&d.ui, "GET", &path, None).await;
    assert_eq!(b["status"], "stopped", "deferred mail must not wake");
    send("urgent", Some(true)).await;
    eventually("urgent mail wakes", Duration::from_secs(5), || async {
        let (_, b) = http(&d.ui, "GET", &path, None).await;
        (b["status"] == "running").then_some(())
    })
    .await;

    // One read-only thread per pair of agents.
    let (status, list) = http(&d.ui, "GET", "/v1/conversations", None).await;
    assert_eq!(status, 200);
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    assert_eq!(list[0]["messages"], 2);
    assert_eq!(list[0]["last"]["priority"], true);
    let (status, thread) = http(
        &d.ui,
        "GET",
        &format!("/v1/conversations/{bob}/{ada}"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let bodies: Vec<_> = thread
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].clone())
        .collect();
    assert_eq!(bodies, [json!("later"), json!("urgent")]);
    let (status, _) = http(&d.ui, "GET", "/v1/conversations/x/y", None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn question_cards_are_answered_once() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let ada = create(&d, json!({"name": "ada"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ada_id: AgentId = ada.parse().unwrap();
    let card = json!({"version": 1, "card": {"title": "Release", "questions": [
        {"id": "day", "prompt": "Which day?", "kind": "single", "allow_custom": true,
         "choices": [{"label": "Thursday", "value": "thu"}, {"label": "Friday", "value": "fri"}]},
        {"id": "where", "prompt": "Where?", "kind": "multi",
         "choices": [{"label": "Email", "value": "email"}, {"label": "Chat", "value": "chat"}]}
    ]}});
    let q = d
        .state
        .store
        .append_event(ada_id, None, "question".into(), card)
        .await
        .unwrap();
    let path = format!("/v1/agents/{ada}/questions/{}/answer", q.id);
    let (status, _) = http(
        &d.ui,
        "POST",
        &path,
        Some(json!({"answers": [
            {"question_id": "day", "values": ["thu", "fri"]},
            {"question_id": "where", "values": ["email"]}
        ]})),
    )
    .await;
    assert_eq!(status, 400, "two picks on a single-choice question");
    let answers = json!({"answers": [
        {"question_id": "day", "custom": "Monday"},
        {"question_id": "where", "values": ["email", "chat"]}
    ]});
    let (status, body) = http(&d.ui, "POST", &path, Some(answers.clone())).await;
    assert_eq!(status, 202, "{body}");
    let (status, _) = http(&d.ui, "POST", &path, Some(answers)).await;
    assert_eq!(status, 409);
    let other = format!("/v1/agents/{ada}/questions/999/answer");
    let (status, _) = http(&d.ui, "POST", &other, Some(json!({"answers": []}))).await;
    assert_eq!(status, 404);

    let (_, msgs) = http(&d.ui, "GET", &format!("/v1/agents/{ada}/messages"), None).await;
    let m = &msgs.as_array().unwrap()[0];
    assert_eq!(m["id"], body["message_id"]);
    assert_eq!(m["sender"]["kind"], "user");
    assert_eq!(
        m["body"],
        "Answers to \"Release\":\n- Which day? Monday\n- Where? Email, Chat"
    );
    let (_, evs) = http(&d.ui, "GET", &format!("/v1/agents/{ada}/events"), None).await;
    let kinds: Vec<_> = evs
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].clone())
        .collect();
    assert!(kinds.contains(&json!("question_answered")), "{kinds:?}");
}

#[tokio::test]
async fn character_avatar_is_part_of_the_profile() {
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let look = json!({"body": "bean", "eyes": "visor", "hue": 212});
    let a = create(&d, json!({"name": "ada", "character": look})).await;
    assert_eq!(a["character"], look);
    let path = format!("/v1/agents/{}", a["id"].as_str().unwrap());
    let (status, b) = http(&d.ui, "PATCH", &path, Some(json!({"description": "x"}))).await;
    assert_eq!((status, &b["character"]), (200, &look), "omitted keeps it");
    let bad = json!({"character": {"body": "bean", "eyes": "dots", "hue": 400}});
    let (status, _) = http(&d.ui, "PATCH", &path, Some(bad)).await;
    assert_eq!(status, 400);
    let (status, b) = http(&d.ui, "PATCH", &path, Some(json!({"character": null}))).await;
    assert_eq!(
        (status, &b["character"]),
        (200, &Value::Null),
        "null resets"
    );
    let fresh = create(&d, json!({"name": "bob"})).await;
    assert_eq!(fresh["character"], Value::Null);
}

fn agent_request(name: &str) -> ferrobot_mcp::AgentRequest {
    ferrobot_mcp::AgentRequest {
        name: name.into(),
        description: "Writes release notes".into(),
        instructions: String::new(),
        character: None,
    }
}

/// Id of the newest pending approval card in the agent's chat.
async fn approval_card(d: &Daemon, agent: &str, after: i64) -> i64 {
    eventually("approval card", Duration::from_secs(5), || async {
        events(d, agent).await.iter().rev().find_map(|e| {
            let id = e["id"].as_i64()?;
            (e["kind"] == "question" && id > after && e["payload"]["approval"].is_object())
                .then_some(id)
        })
    })
    .await
}

async fn decide(d: &Daemon, agent: &str, qid: i64, value: &str) -> (u16, Value) {
    http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{agent}/questions/{qid}/answer"),
        Some(json!({"answers": [{"question_id": "q1", "values": [value]}]})),
    )
    .await
}

#[tokio::test]
async fn agents_create_agents_only_with_approval() {
    use ferrobot_mcp::AgentRequestOutcome;
    let backend = Arc::new(StatefulBackend::default());
    let d = start_with(backend, None, "127.0.0.1:0", |s| {
        s.approval_timeout = Duration::from_secs(2);
    })
    .await;
    let (_, provider) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(json!({"name": "local", "kind": "ollama", "default_model": "llama3"})),
    )
    .await;
    let ada = create(
        &d,
        json!({"name": "ada", "provider_id": provider["id"], "model": "qwen3"}),
    )
    .await;
    assert!(ada["created_by"].is_null(), "operator-created: {ada}");
    let ada = ada["id"].as_str().unwrap().to_owned();
    let ada_id: AgentId = ada.parse().unwrap();

    // Approve: the agent exists and remembers who created it.
    let st = d.state.clone();
    let req = tokio::spawn(async move { st.request_agent(ada_id, agent_request("scout")).await });
    let q1 = approval_card(&d, &ada, 0).await;
    let card = events(&d, &ada)
        .await
        .into_iter()
        .find(|e| e["id"] == q1)
        .unwrap();
    assert_eq!(card["payload"]["approval"]["name"], "scout");
    assert_eq!(
        card["payload"]["card"]["questions"][0]["choices"][0]["value"],
        "approve"
    );
    let (status, body) = decide(&d, &ada, q1, "approve").await;
    assert_eq!(status, 202, "{body}");
    assert!(
        body["message_id"].is_null(),
        "approvals do not message the agent"
    );
    let AgentRequestOutcome::Created { id, name } = req.await.unwrap().unwrap() else {
        panic!("expected created");
    };
    assert_eq!(name, "scout");
    let (status, scout) = http(&d.ui, "GET", &format!("/v1/agents/{id}"), None).await;
    assert_eq!(status, 200);
    assert_eq!(scout["created_by"], ada.as_str());
    assert_eq!(
        scout["provider_id"], provider["id"],
        "inherits the provider"
    );
    assert_eq!(scout["model"], "qwen3");
    assert_eq!(scout["description"], "Writes release notes");
    let (_, msgs) = http(&d.ui, "GET", &format!("/v1/agents/{ada}/messages"), None).await;
    assert_eq!(msgs.as_array().unwrap().len(), 0, "{msgs}");

    // Decline: nothing is created.
    let st = d.state.clone();
    let req = tokio::spawn(async move { st.request_agent(ada_id, agent_request("echo")).await });
    let q2 = approval_card(&d, &ada, q1).await;
    let (status, _) = decide(&d, &ada, q2, "decline").await;
    assert_eq!(status, 202);
    assert!(matches!(
        req.await.unwrap().unwrap(),
        AgentRequestOutcome::Declined { .. }
    ));

    // No answer in time: declined, and the card is locked.
    let outcome = d
        .state
        .request_agent(ada_id, agent_request("echo"))
        .await
        .unwrap();
    let AgentRequestOutcome::Declined { reason } = outcome else {
        panic!("expected declined");
    };
    assert!(reason.contains("in time"), "{reason}");
    let q3 = approval_card(&d, &ada, q2).await;
    let (status, _) = decide(&d, &ada, q3, "approve").await;
    assert_eq!(status, 409, "already answered (declined on timeout)");

    let (_, all) = http(&d.ui, "GET", "/v1/agents", None).await;
    let names: Vec<_> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("ada"), json!("scout")]);

    // Invalid or taken names fail before the operator is asked.
    let err = d
        .state
        .request_agent(ada_id, agent_request("scout"))
        .await
        .unwrap_err();
    assert!(err.contains("taken"), "{err}");
    let err = d
        .state
        .request_agent(ada_id, agent_request("Bad Name"))
        .await
        .unwrap_err();
    assert!(err.contains("name"), "{err}");
}

#[tokio::test]
async fn agent_limit_applies_to_api_and_agents() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start_with(backend, None, "127.0.0.1:0", |s| s.max_agents = 2).await;
    let ada = create(&d, json!({"name": "ada"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    create(&d, json!({"name": "bob"})).await;
    let (status, body) = http(&d.ui, "POST", "/v1/agents", Some(json!({"name": "cy"}))).await;
    assert_eq!(status, 409, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("limit"),
        "{body}"
    );
    let err = d
        .state
        .request_agent(ada.parse().unwrap(), agent_request("cy"))
        .await
        .unwrap_err();
    assert!(err.contains("at most 2 agents"), "{err}");
    assert!(
        !events(&d, &ada)
            .await
            .iter()
            .any(|e| e["kind"] == "question"),
        "no approval card when the limit is reached"
    );
}

/// Fake upstream for any provider: echoes the method, path and credentials it saw.
async fn echo_upstream() -> String {
    async fn handler(request: axum::extract::Request) -> axum::Json<Value> {
        let header = |name: &str| {
            request
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        axum::Json(json!({
            "method": request.method().as_str(),
            "path": request.uri().path(),
            "authorization": header("authorization"),
            "x_api_key": header("x-api-key"),
            "accounts": request.headers().get_all("chatgpt-account-id").iter()
                .filter_map(|v| v.to_str().ok()).collect::<Vec<_>>(),
        }))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().fallback(handler);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn model_call(d: &Daemon, path: &str, auth: (&str, &str)) -> (u16, Value) {
    let (status, _, out) = http_raw(&d.agents, "POST", path, &[auth], b"{}").await;
    (status, serde_json::from_slice(&out).unwrap_or(Value::Null))
}

#[tokio::test]
async fn providers_keep_keys_in_the_daemon() {
    let upstream = echo_upstream().await;
    let backend = Arc::new(StatefulBackend::default());
    let d = start_with(backend.clone(), None, "127.0.0.1:0", |s| {
        s.driver = "claude".into();
        s.gcloud_token_cmd = vec!["echo".into(), "google-token".into()];
    })
    .await;

    let (status, openai) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(
            json!({"name": "oai", "kind": "openai", "base_url": format!("{upstream}/v1"),
                    "default_model": "gpt-test", "api_key": "sk-openai-secret"}),
        ),
    )
    .await;
    assert_eq!(status, 201, "{openai}");
    assert_eq!(openai["has_key"], true);
    assert!(!openai.to_string().contains("sk-openai-secret"));
    let (status, err) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(json!({"name": "bad", "kind": "bedrock"})),
    )
    .await;
    assert_eq!(status, 400, "region is required: {err}");

    // OpenAI-compatible agents get the openai driver and only their own token.
    let pid = openai["id"].as_str().unwrap();
    create(&d, json!({"name": "olly", "provider_id": pid})).await;
    let env = backend.envs.lock().unwrap()["olly"].clone();
    let token = env["FERROBOT_AGENT_TOKEN"].clone();
    assert_eq!(env["FERROBOT_DRIVER"], "openai");
    assert_eq!(env["FERROBOT_MODEL"], "gpt-test");
    assert!(env["FERROBOT_OPENAI_BASE_URL"].ends_with("/model"));
    assert!(!env.values().any(|v| v.contains("sk-openai-secret")));
    let bearer = format!("Bearer {token}");
    let (status, seen) =
        model_call(&d, "/model/chat/completions", ("authorization", &bearer)).await;
    assert_eq!(status, 200, "{seen}");
    assert_eq!(seen["path"], "/v1/chat/completions");
    assert_eq!(seen["authorization"], "Bearer sk-openai-secret");

    // Bedrock: invocations go to the runtime with the Bedrock API key.
    let (_, bedrock) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(
            json!({"name": "br", "kind": "bedrock", "region": "eu-central-1",
                    "base_url": upstream, "api_key": "bedrock-key"}),
        ),
    )
    .await;
    create(
        &d,
        json!({"name": "bert", "provider_id": bedrock["id"], "model": "eu.anthropic.claude-x"}),
    )
    .await;
    let env = backend.envs.lock().unwrap()["bert"].clone();
    assert_eq!(env["FERROBOT_DRIVER"], "claude");
    assert_eq!(env["CLAUDE_CODE_USE_BEDROCK"], "1");
    assert_eq!(env["AWS_REGION"], "eu-central-1");
    assert_eq!(env["AWS_BEARER_TOKEN_BEDROCK"], env["FERROBOT_AGENT_TOKEN"]);
    assert_eq!(env["FERROBOT_MODEL"], "eu.anthropic.claude-x");
    let bearer = format!("Bearer {}", env["FERROBOT_AGENT_TOKEN"]);
    let (_, seen) = model_call(
        &d,
        "/model/model/eu.anthropic.claude-x/invoke",
        ("authorization", &bearer),
    )
    .await;
    assert_eq!(seen["path"], "/model/eu.anthropic.claude-x/invoke");
    assert_eq!(seen["authorization"], "Bearer bedrock-key");

    // Vertex: the daemon host's gcloud token, never the agent's.
    let (_, vertex) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(
            json!({"name": "vx", "kind": "vertex", "region": "us-east5", "project": "p1",
                    "base_url": upstream}),
        ),
    )
    .await;
    assert_eq!(vertex["has_key"], false);
    create(&d, json!({"name": "vera", "provider_id": vertex["id"]})).await;
    let env = backend.envs.lock().unwrap()["vera"].clone();
    assert_eq!(env["CLAUDE_CODE_USE_VERTEX"], "1");
    assert_eq!(env["ANTHROPIC_VERTEX_PROJECT_ID"], "p1");
    assert_eq!(env["CLOUD_ML_REGION"], "us-east5");
    let (_, seen) = model_call(
        &d,
        "/model/projects/p1/locations/us-east5/publishers/anthropic/models/m:streamRawPredict",
        ("x-api-key", &env["FERROBOT_AGENT_TOKEN"]),
    )
    .await;
    assert_eq!(seen["authorization"], "Bearer google-token");
    assert_eq!(seen["x_api_key"], Value::Null);

    // Removing the key makes the proxy refuse; the agent is told why.
    let (status, _) = http(
        &d.ui,
        "PATCH",
        &format!("/v1/providers/{pid}"),
        Some(json!({"api_key": null})),
    )
    .await;
    assert_eq!(status, 200);
    let bearer = format!("Bearer {token}");
    let (status, err) = model_call(&d, "/model/chat/completions", ("authorization", &bearer)).await;
    assert_eq!(status, 503, "{err}");

    // Providers in use cannot be deleted; switching the model rebuilds the sandbox.
    let (status, _) = http(&d.ui, "DELETE", &format!("/v1/providers/{pid}"), None).await;
    assert_eq!(status, 409);
    let olly = backend.envs.lock().unwrap()["olly"]["FERROBOT_AGENT_TOKEN"].clone();
    let agents = d.state.store.list_agents().await.unwrap();
    let olly_id = agents
        .iter()
        .find(|a| a.name.as_str() == "olly")
        .unwrap()
        .id;
    let (status, view) = http(
        &d.ui,
        "PATCH",
        &format!("/v1/agents/{olly_id}"),
        Some(json!({"model": "gpt-other"})),
    )
    .await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["model"], "gpt-other");
    let env = backend.envs.lock().unwrap()["olly"].clone();
    assert_eq!(env["FERROBOT_MODEL"], "gpt-other");
    assert_ne!(
        env["FERROBOT_AGENT_TOKEN"], olly,
        "a new sandbox got a new token"
    );
    let (status, _) = http(
        &d.ui,
        "PATCH",
        &format!("/v1/agents/{olly_id}"),
        Some(json!({"provider_id": null})),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = http(&d.ui, "DELETE", &format!("/v1/providers/{pid}"), None).await;
    assert_eq!(status, 204);
    let (status, err) = http(
        &d.ui,
        "POST",
        "/v1/agents",
        Some(json!({"name": "ghost", "provider_id": pid})),
    )
    .await;
    assert_eq!(status, 400, "{err}");
}

/// Fake host `codex app-server`: answers by method, one JSON object per line.
const FAKE_CODEX: &str = r#"#!/bin/sh
JWT='eyJhbGciOiJub25lIn0.eyJleHAiOjQxMDI0NDQ4MDAsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2N0LTEyMyJ9fQ.sig'
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{"id":%s,"result":{}}\n' "$id" ;;
    *'"getAuthStatus"'*) printf '{"id":%s,"result":{"authMethod":"chatgpt","authToken":"%s"}}\n' "$id" "$JWT" ;;
    *'"account/login/start"'*)
      printf '{"id":%s,"result":{"type":"chatgptDeviceCode","loginId":"l1","verificationUrl":"https://auth.example/device","userCode":"ABCD-1234"}}\n' "$id"
      printf '{"method":"account/login/completed","params":{"loginId":"l1","success":true,"error":null}}\n' ;;
  esac
done
"#;

#[tokio::test]
async fn codex_providers_use_a_key_or_the_hosts_chatgpt_login() {
    let upstream = echo_upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("codex");
    std::fs::write(&bin, FAKE_CODEX).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let backend = Arc::new(StatefulBackend::default());
    let (home, chatgpt) = (dir.path().join("homes"), format!("{upstream}/codex"));
    let d = start_with(backend.clone(), None, "127.0.0.1:0", |s| {
        s.driver = "claude".into();
        s.codex_bin = bin.display().to_string();
        s.codex_home = home;
        s.chatgpt_upstream = chatgpt;
    })
    .await;

    // With an API key: the OpenAI Responses API.
    let (_, keyed) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(
            json!({"name": "cx-key", "kind": "codex", "base_url": format!("{upstream}/v1"),
                    "api_key": "sk-codex-key", "default_model": "gpt-5-codex"}),
        ),
    )
    .await;
    create(&d, json!({"name": "cody", "provider_id": keyed["id"]})).await;
    let env = backend.envs.lock().unwrap()["cody"].clone();
    assert_eq!(env["FERROBOT_DRIVER"], "codex");
    assert!(env["FERROBOT_CODEX_BASE_URL"].ends_with("/model"));
    assert_eq!(env["FERROBOT_MODEL"], "gpt-5-codex");
    let bearer = format!("Bearer {}", env["FERROBOT_AGENT_TOKEN"]);
    let (_, seen) = model_call(&d, "/model/responses", ("authorization", &bearer)).await;
    assert_eq!(seen["path"], "/v1/responses");
    assert_eq!(seen["authorization"], "Bearer sk-codex-key");

    // Without a key: the host's ChatGPT sign-in, tokens never leave the daemon.
    let (_, chat) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(json!({"name": "cx-chatgpt", "kind": "codex", "default_model": "gpt-5-codex"})),
    )
    .await;
    let login = format!(
        "/v1/providers/{}/chatgpt-login",
        chat["id"].as_str().unwrap()
    );
    let (status, code) = http(&d.ui, "POST", &login, None).await;
    assert_eq!(status, 200, "{code}");
    assert_eq!(code["user_code"], "ABCD-1234");
    eventually("ChatGPT sign-in", Duration::from_secs(5), || async {
        let (_, s) = http(&d.ui, "GET", &login, None).await;
        (s["state"] == "signed_in").then_some(())
    })
    .await;
    create(&d, json!({"name": "chad", "provider_id": chat["id"]})).await;
    let env = backend.envs.lock().unwrap()["chad"].clone();
    assert!(!env.values().any(|v| v.contains("eyJhbGciOiJub25lIn0")));
    let bearer = format!("Bearer {}", env["FERROBOT_AGENT_TOKEN"]);
    let (status, _, out) = http_raw(
        &d.agents,
        "POST",
        "/model/responses",
        &[
            ("authorization", &bearer),
            ("chatgpt-account-id", "someone-else"),
        ],
        b"{}",
    )
    .await;
    let seen: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(status, 200, "{seen}");
    assert_eq!(seen["path"], "/codex/responses");
    assert!(
        seen["authorization"]
            .as_str()
            .unwrap()
            .starts_with("Bearer eyJhbGciOiJub25lIn0.")
    );
    assert_eq!(
        seen["accounts"],
        json!(["acct-123"]),
        "the agent cannot pick the account"
    );

    // Sign-in is only for codex providers.
    let other = format!(
        "/v1/providers/{}/chatgpt-login",
        keyed["id"].as_str().unwrap()
    );
    let (_, openai) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(json!({"name": "oa", "kind": "openai", "api_key": "k"})),
    )
    .await;
    let not_codex = format!(
        "/v1/providers/{}/chatgpt-login",
        openai["id"].as_str().unwrap()
    );
    assert_eq!(http(&d.ui, "POST", &not_codex, None).await.0, 409);
    assert_eq!(http(&d.ui, "GET", &other, None).await.0, 200);
}

#[tokio::test]
async fn idle_agents_pause_before_they_stop() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start_with(backend.clone(), None, "127.0.0.1:0", |s| {
        s.warm_timeout = Some(Duration::from_millis(200));
        s.idle_timeout = Some(Duration::from_millis(600));
    })
    .await;
    let id = create(&d, json!({"name": "wren"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let _ws = connect_runner(&d.agents, &backend.token("wren")).await;
    let agent = || async {
        http(&d.ui, "GET", &format!("/v1/agents/{id}"), None)
            .await
            .1
    };
    let count = |what: &str| backend.calls().iter().filter(|c| *c == what).count();

    tokio::time::sleep(Duration::from_millis(300)).await;
    ferrobot_daemon::supervisor::check_all(&d.state).await;
    let a = agent().await;
    assert_eq!(
        (&a["status"], &a["presence"]),
        (&json!("paused"), &json!("sleeping"))
    );
    let (_, info) = http(&d.ui, "GET", &format!("/v1/agents/{id}/sandbox"), None).await;
    assert_eq!(info["state"], "paused");
    assert_eq!(info["warm_after_s"], 0);
    assert_eq!(count("pause wren"), 1);

    // A paused sandbox is not a crash: the supervisor leaves it alone...
    ferrobot_daemon::supervisor::check_all(&d.state).await;
    assert_eq!((count("wake wren"), count("create wren")), (0, 1));
    // ...until the cold timeout, when it stops.
    tokio::time::sleep(Duration::from_millis(400)).await;
    ferrobot_daemon::supervisor::check_all(&d.state).await;
    assert_eq!(agent().await["status"], "stopped");
    assert_eq!(count("stop wren"), 1);

    // An operator can pause on purpose; a message resumes it in place.
    http(&d.ui, "POST", &format!("/v1/agents/{id}/wake"), None).await;
    let (status, _) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{id}/stop?level=warm"),
        None,
    )
    .await;
    assert_eq!(status, 204);
    assert_eq!(agent().await["status"], "paused");
    let (status, _) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{id}/messages"),
        Some(json!({"body": "resume"})),
    )
    .await;
    assert_eq!(status, 202);
    eventually("resume", Duration::from_secs(5), || async {
        (agent().await["status"] == "running").then_some(())
    })
    .await;
    assert_eq!(
        count("create wren"),
        1,
        "resumed, not re-created: {:?}",
        backend.calls()
    );
    assert_eq!(count("wake wren"), 2);
}
