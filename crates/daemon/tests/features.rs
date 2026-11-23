//! Daemon features against a stateful fake backend and fake upstreams. No Node, no Docker.

#![allow(
    clippy::too_many_lines,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

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
use ferrobot_daemon::files;
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
    /// Sandbox names whose disk went to the archive.
    archived: Mutex<std::collections::HashSet<String>>,
    /// The image each existing sandbox was created from.
    images: Mutex<HashMap<String, Option<String>>>,
    /// Held by a test to keep `build_image` running.
    build_gate: tokio::sync::Mutex<()>,
    /// Path and length of every `write_file`.
    written: Mutex<Vec<(String, usize)>>,
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
        if self.archived.lock().unwrap().remove(&spec.name) {
            self.log(format!("unarchive {}", spec.name));
        }
        self.envs
            .lock()
            .unwrap()
            .insert(spec.name.clone(), spec.env.clone());
        self.images
            .lock()
            .unwrap()
            .insert(spec.name.clone(), spec.image.clone());
        self.set(&spec.name, SandboxState::Running);
        self.log(format!("create {}", spec.name));
        Ok(SandboxHandle {
            backend: "fake".into(),
            id: spec.name.clone(),
        })
    }
    fn supports_image_builds(&self) -> bool {
        true
    }
    async fn build_image(&self, packages: &[String]) -> Result<Option<String>, SandboxError> {
        let _gate = self.build_gate.lock().await;
        let set = ferrobot_sandbox::package_set(packages)?;
        self.log(format!("build {}", set.join(" ")));
        Ok((!set.is_empty()).then(|| format!("fake:clis-{}", set.join("-"))))
    }
    async fn is_stale(&self, h: &SandboxHandle, image: Option<&str>) -> Result<bool, SandboxError> {
        let images = self.images.lock().unwrap();
        Ok(images.get(&h.id).is_some_and(|i| i.as_deref() != image))
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
        if p.as_str().contains("missing") {
            return Err(SandboxError::NotFound(p.to_string()));
        }
        Ok(format!("content of {p}").into_bytes())
    }
    async fn write_file(
        &self,
        _: &SandboxHandle,
        p: &SandboxPath,
        bytes: &[u8],
    ) -> Result<(), SandboxError> {
        self.written
            .lock()
            .unwrap()
            .push((p.to_string(), bytes.len()));
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
    async fn delete_archive(&self, name: &str) -> Result<(), SandboxError> {
        self.log(format!("delete_archive {name}"));
        Ok(())
    }
    async fn delete_snapshot(&self, snapshot: &str) -> Result<(), SandboxError> {
        self.log(format!("delete_snapshot {snapshot}"));
        Ok(())
    }
    async fn wake(&self, h: &SandboxHandle) -> Result<(), SandboxError> {
        if self.archived.lock().unwrap().contains(&h.id) {
            return Err(SandboxError::NotFound(h.id.clone()));
        }
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
        } else if level == SleepLevel::Archived {
            self.states.lock().unwrap().remove(&h.id);
            self.archived.lock().unwrap().insert(h.id.clone());
            self.log(format!("archive {}", h.id));
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
        self.images.lock().unwrap().remove(&h.id);
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
    connect_runner_with(agents, token, false).await
}

/// A runner connection right after its `hello`.
async fn open_runner(agents: &str, token: &str, protocol_version: u32, steering: bool) -> Ws {
    let mut req = format!("ws://{agents}/runner/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let hello = RunnerFrame::Hello {
        protocol_version,
        runner_version: "test".into(),
        driver: "fake".into(),
        session_id: None,
        steering,
    };
    ws.send(Message::text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();
    ws
}

async fn connect_runner_with(agents: &str, token: &str, steering: bool) -> Ws {
    let mut ws = open_runner(agents, token, ferrobot_core::PROTOCOL_VERSION, steering).await;
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
    assert_eq!(a["pinned"], false);
    let pin = |pinned: bool| {
        let (ui, path) = (d.ui.clone(), format!("/v1/agents/{id}"));
        async move { http(&ui, "PATCH", &path, Some(json!({ "pinned": pinned }))).await }
    };
    let (status, pinned) = pin(true).await;
    assert_eq!(
        (status, &pinned["pinned"], &pinned["description"]),
        (200, &json!(true), &json!("Research lead"))
    );
    assert_eq!(pin(false).await.1["pinned"], false);

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
            ..
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
        steered: vec![],
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

    // Global memory: same shape, not tied to an agent.
    let body = json!({"key": "home", "content": "Operator lives in Hamburg"});
    let (status, _) = http(&d.ui, "POST", "/v1/memory", Some(body)).await;
    assert_eq!(status, 204);
    let (_, hits) = http(&d.ui, "GET", "/v1/memory?q=hamburg", None).await;
    assert_eq!(hits[0]["key"], "home", "{hits}");
    let (_, own) = http(&d.ui, "GET", &format!("{mem}?q=hamburg"), None).await;
    assert_eq!(own, json!([]));
    let (status, _) = http(&d.ui, "DELETE", "/v1/memory/home", None).await;
    assert_eq!(status, 204);
    let (status, _) = http(&d.ui, "DELETE", "/v1/memory/home", None).await;
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
async fn cli_image_reaches_new_and_woken_sandboxes() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let ui = d.ui.as_str();
    let put = |packages: Value| http(ui, "PUT", "/v1/clis", Some(json!({ "packages": packages })));
    let many: Vec<String> = (0..33).map(|i| format!("p{i}")).collect();
    for bad in [
        json!(["-o"]),
        json!(["jq; id"]),
        json!(["Jq"]),
        json!(["a/b"]),
        json!(many),
    ] {
        let (status, body) = put(bad.clone()).await;
        assert_eq!(status, 400, "{bad}: {body}");
    }
    let (status, v) = http(&d.ui, "GET", "/v1/clis", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        v,
        json!({"packages": [], "status": "idle", "error": null, "supported": true})
    );

    let running = create(&d, json!({"name": "ada"})).await;
    let asleep = create(&d, json!({"name": "bob"})).await;
    let (status, _) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{}/stop", asleep["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, 204);

    let gate = backend.build_gate.lock().await;
    let (status, v) = put(json!(["tree", "jq", "jq"])).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        (v["packages"].clone(), v["status"].clone()),
        (json!(["jq", "tree"]), json!("building"))
    );
    let (status, _) = put(json!(["jq"])).await;
    assert_eq!(status, 409, "one build at a time");
    drop(gate);
    eventually("the image build", Duration::from_secs(5), || async {
        let (_, v) = http(&d.ui, "GET", "/v1/clis", None).await;
        (v["status"] == "ready").then_some(())
    })
    .await;
    let image = Some("fake:clis-jq-tree".to_owned());
    let image_of = |name: &str| backend.images.lock().unwrap().get(name).cloned();

    assert_eq!(
        image_of("ada"),
        Some(None),
        "a running sandbox is left alone"
    );
    create(&d, json!({"name": "cy"})).await;
    assert_eq!(image_of("cy"), Some(image.clone()));

    let wake = |agent: &Value| {
        let path = format!("/v1/agents/{}/wake", agent["id"].as_str().unwrap());
        async move { http(ui, "POST", &path, None).await.0 }
    };
    assert_eq!(wake(&asleep).await, 204);
    assert_eq!(image_of("bob"), Some(image.clone()));
    let calls = backend.calls();
    let destroyed = calls
        .iter()
        .position(|c| c == "destroy bob purge=false")
        .expect("recreated");
    assert_eq!(calls[destroyed + 1], "create bob", "{calls:?}");
    assert_eq!(wake(&asleep).await, 204);
    assert_eq!(
        backend.calls().last().unwrap(),
        "wake bob",
        "a current sandbox just wakes"
    );
    assert_eq!(wake(&running).await, 204);
    assert_eq!(image_of("ada"), Some(image), "the next wake switches it");

    let (status, _) = put(json!([])).await;
    assert_eq!(status, 200);
    eventually("the base image", Duration::from_secs(5), || async {
        let (_, v) = http(&d.ui, "GET", "/v1/clis", None).await;
        (v["status"] == "idle").then_some(())
    })
    .await;
    assert_eq!(wake(&asleep).await, 204);
    assert_eq!(image_of("bob"), Some(None));
    assert!(
        backend.calls().iter().all(|c| !c.contains("purge=true")),
        "home volumes are never deleted"
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
    // Health stays open and says whether agents can start (the desktop app checks it for Docker).
    assert_eq!(get("/v1/health", "").await.0, 200);
    let (status, health) = http(&d.ui, "GET", "/v1/health", None).await;
    assert_eq!(status, 200);
    assert_eq!(health["sandbox_ready"], true);
    assert_eq!(health["sandbox_error"], Value::Null);
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
    let (status, _, _) = http_raw(
        &d.ui,
        "PUT",
        "/v1/settings",
        &[("Authorization", ""), ("Content-Type", "application/json")],
        br#"{"auto_mode":true}"#,
    )
    .await;
    assert_eq!(status, 401, "only the operator turns on Auto mode");
}

#[cfg(debug_assertions)]
fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

/// Update steps that pull one new commit, fail the first daemon build and record the restart.
#[cfg(debug_assertions)]
#[derive(Debug, Default)]
struct FakeSteps {
    calls: Mutex<Vec<String>>,
    restarts: Mutex<usize>,
}

#[cfg(debug_assertions)]
#[async_trait]
impl ferrobot_daemon::update::Runner for FakeSteps {
    async fn run(
        &self,
        dir: &std::path::Path,
        cmd: &[&str],
        log: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(), String> {
        let cmd = cmd.join(" ").replace("npm.cmd", "npm");
        log(format!("ran {cmd}"));
        let runs = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(cmd.clone());
            calls.iter().filter(|c| **c == cmd).count()
        };
        match (cmd.as_str(), runs) {
            ("git pull --ff-only", 1) => {
                git(dir, &["commit", "-q", "--allow-empty", "-m", "pulled"]);
                Ok(())
            }
            ("cargo build -p ferrobot-daemon", 1) => Err("error[E0425]: cannot find value".into()),
            _ => Ok(()),
        }
    }

    fn restart(&self, _: &std::path::Path) -> Result<(), String> {
        *self.restarts.lock().unwrap() += 1;
        Ok(())
    }
}

#[tokio::test]
async fn diagnostics_need_the_token_and_redact_the_log() {
    use tracing_subscriber::layer::SubscriberExt;

    let ring = ferrobot_daemon::logs::LogRing::default();
    let daemon_key = "sk-ant-daemon-key-0123456789abcdef";
    let d = start_with(
        Arc::new(StatefulBackend::default()),
        None,
        "127.0.0.1:0",
        |s| {
            s.log = ring.clone();
            s.anthropic_api_key = Some(daemon_key.into());
            s.data_dir = "/tmp/ferrobot-data".into();
        },
    )
    .await;
    for path in ["/v1/diagnostics", "/v1/diagnostics/log"] {
        let (status, _, _) = http_raw(&d.ui, "GET", path, &[("Authorization", "")], b"").await;
        assert_eq!(status, 401, "{path}");
    }

    http(&d.ui, "POST", "/v1/agents", Some(json!({"name": "kim"}))).await;
    let (status, info) = http(&d.ui, "GET", "/v1/diagnostics", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        (&info["backend"], &info["data_dir"], &info["storage"]),
        (
            &json!("fake"),
            &json!("/tmp/ferrobot-data"),
            &json!("memory")
        )
    );
    assert_eq!(
        (
            &info["agents"],
            &info["anthropic_key"],
            &info["sandbox_ready"]
        ),
        (&json!(1), &json!(true), &json!(true))
    );
    assert_eq!(
        info["runner_image"],
        Value::Null,
        "the fake backend has no images"
    );
    let body = info.to_string();
    assert!(!body.contains(daemon_key) && !body.contains(common::TEST_TOKEN));

    let planted = "upstream refused sk-ant-api03-planted0123456789 for Bearer planted-bearer-token";
    let subscriber =
        tracing_subscriber::registry().with(ferrobot_daemon::logs::layer(ring.clone()));
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("first line");
        tracing::warn!(
            url = "/v1/ws?access_token=planted-query-token-0123",
            "{planted}"
        );
    });
    let (status, log) = http(&d.ui, "GET", "/v1/diagnostics/log", None).await;
    assert_eq!(status, 200);
    let lines: Vec<&str> = log["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].ends_with("first line"), "{lines:?}");
    assert!(
        lines[1].contains("upstream refused [REDACTED] for Bearer [REDACTED]"),
        "{lines:?}"
    );
    assert!(!lines[1].contains("planted"), "{lines:?}");
    let (_, last) = http(&d.ui, "GET", "/v1/diagnostics/log?lines=1", None).await;
    assert_eq!(last["lines"].as_array().unwrap().len(), 1);
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn source_update_needs_the_token_and_reports_its_steps() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("crates/daemon")).unwrap();
    std::fs::create_dir_all(repo.join("target/debug")).unwrap();
    std::fs::write(
        repo.join("crates/daemon/Cargo.toml"),
        "[package]\nname = \"ferrobot-daemon\"\n",
    )
    .unwrap();
    std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "init"]);
    let steps = Arc::new(FakeSteps::default());
    let source =
        ferrobot_daemon::update::Source::detect(repo.join("target/debug/ferrobotd"), steps.clone())
            .unwrap();
    let d = start_with(
        Arc::new(StatefulBackend::default()),
        None,
        "127.0.0.1:0",
        |s| s.source_update = Some(source),
    )
    .await;
    for method in ["GET", "POST"] {
        let (status, _, _) =
            http_raw(&d.ui, method, "/v1/update", &[("Authorization", "")], b"").await;
        assert_eq!(status, 401, "{method}");
    }
    let (status, info) = http(&d.ui, "GET", "/v1/update", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        (&info["source_build"], &info["state"]),
        (&json!(true), &json!("idle"))
    );
    let started = info["commit"].clone();
    let reached = |want: &'static str| {
        let ui = d.ui.clone();
        eventually(want, Duration::from_secs(10), move || {
            let ui = ui.clone();
            async move {
                let (_, info) = http(&ui, "GET", "/v1/update", None).await;
                (info["state"] == want).then_some(info)
            }
        })
    };

    let (status, info) = http(&d.ui, "POST", "/v1/update", None).await;
    assert_eq!((status, info["state"].as_str()), (202, Some("running")));
    let failed = reached("failed").await;
    assert_eq!(failed["step"], "cargo build -p ferrobot-daemon");
    assert!(
        failed["error"].as_str().unwrap().contains("E0425"),
        "{failed}"
    );
    assert!(
        failed["log"]
            .as_array()
            .unwrap()
            .contains(&json!("ran npm run build"))
    );
    assert_eq!(failed["commit"], started, "the commit the daemon runs");

    // The retry pulls nothing new but still rebuilds: the daemon runs an older commit.
    let (status, _) = http(&d.ui, "POST", "/v1/update", None).await;
    assert_eq!(status, 202);
    reached("restarting").await;
    assert_eq!(*steps.restarts.lock().unwrap(), 1);
    let daemon = [
        "git pull --ff-only",
        "npm ci",
        "npm run build",
        "cargo build -p ferrobot-daemon",
    ];
    let mut want = [daemon, daemon].concat();
    want.push("cargo build");
    assert_eq!(*steps.calls.lock().unwrap(), want);
    let (status, info) = http(&d.ui, "POST", "/v1/update", None).await;
    assert_eq!(status, 409, "{info}");
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
async fn a_turns_trace_lines_up_its_model_and_tool_calls() {
    use ferrobot_core::protocol::TurnEvent;
    let upstream = fake_upstream().await;
    let backend = Arc::new(StatefulBackend::default());
    let d = start_with(backend.clone(), None, "127.0.0.1:0", |s| {
        s.driver = "claude".into();
        s.anthropic_api_key = Some("sk-real-secret".into());
        s.anthropic_upstream = upstream;
    })
    .await;
    let id = create(&d, json!({"name": "tia"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let token = backend.envs.lock().unwrap()["tia"]["FERROBOT_AGENT_TOKEN"].clone();
    let mut ws = connect_runner(&d.agents, &token).await;
    let path = format!("/v1/agents/{id}/messages");
    http(&d.ui, "POST", &path, Some(json!({"body": "plan it"}))).await;
    let (turn_id, _, _) = next_turn(&mut ws).await;

    let model = |body: &'static [u8]| {
        let (agents, token) = (d.agents.clone(), token.clone());
        async move {
            let headers = [
                ("x-api-key", token.as_str()),
                ("content-type", "application/json"),
            ];
            http_raw(&agents, "POST", "/model/v1/messages", &headers, body)
                .await
                .0
        }
    };
    assert_eq!(
        model(br#"{"model":"claude-test","stream":true}"#).await,
        200
    );
    for event in [
        TurnEvent::ToolUse {
            id: Some("a".into()),
            name: "read_file".into(),
            input: json!({"path": "brief.md"}),
        },
        TurnEvent::ToolResult {
            id: Some("a".into()),
            output: "no such file".into(),
            is_error: true,
        },
    ] {
        let frame = RunnerFrame::Event {
            turn_id: turn_id.clone(),
            event,
        };
        ws.send(Message::text(serde_json::to_string(&frame).unwrap()))
            .await
            .unwrap();
    }
    answer(&mut ws, turn_id.clone(), "done").await;
    let agent: AgentId = id.parse().unwrap();
    eventually("the turn to end", Duration::from_secs(5), || async {
        (!d.state.hub.is_working(agent)).then_some(())
    })
    .await;
    // A call outside any turn stays out of every trace.
    assert_eq!(model(br#"{"model":"elsewhere","stream":true}"#).await, 200);

    let trace = format!("/v1/agents/{id}/turns/{turn_id}/trace");
    let t = eventually("the finished trace", Duration::from_secs(5), || async {
        let (_, t) = http(&d.ui, "GET", &trace, None).await;
        (t["outcome"] == "completed" && t["spans"].as_array().is_some_and(|s| s.len() == 2))
            .then_some(t)
    })
    .await;
    assert_eq!(t["models_tracked"], true);
    let spans = t["spans"].as_array().unwrap();
    assert_eq!(
        (&spans[0]["kind"], &spans[0]["name"]),
        (&json!("model"), &json!("claude-test"))
    );
    assert_eq!(
        (&spans[1]["kind"], &spans[1]["name"], &spans[1]["error"]),
        (&json!("tool"), &json!("read_file"), &json!("no such file"))
    );
    assert_eq!(spans[1]["input"], json!({"path": "brief.md"}));
    let missing = format!("/v1/agents/{id}/turns/nope/trace");
    assert_eq!(http(&d.ui, "GET", &missing, None).await.0, 404);
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
    // The key's other powers and paths outside the base stay out of reach.
    for (method, path) in [
        ("GET", "/model/v1/files"),
        ("DELETE", "/model/v1/messages"),
        ("GET", "/model/v1/%252e%252e/%252e%252e/escape"),
        ("GET", "/model/v1/.%09./x"),
    ] {
        let (status, _, _) = http_raw(&d.agents, method, path, &[("x-api-key", &token)], b"").await;
        assert_eq!(status, 404, "{method} {path}");
    }

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
    assert_eq!(status, 400, "a setup problem, not worth retrying");
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
        tools: None,
        connectors: None,
    }
}

#[tokio::test]
async fn tool_profiles_bind_new_agents_and_reach_the_runner() {
    use ferrobot_core::ToolGroup::{CreateAgents, Files, Memory, Shell, Team, Web};
    use ferrobot_mcp::{AgentRequest, AgentRequestOutcome};
    let backend = Arc::new(StatefulBackend::default());
    let d = start_with(backend.clone(), None, "127.0.0.1:0", |s| {
        s.approval_timeout = Duration::from_secs(5);
    })
    .await;

    // The operator sets any profile; omitted means every group.
    let ada = create(
        &d,
        json!({"name": "ada", "tools": ["files", "team", "memory", "create_agents"]}),
    )
    .await;
    assert_eq!(
        ada["tools"],
        json!(["team", "memory", "create_agents", "files"])
    );
    let max = create(&d, json!({"name": "max"})).await;
    assert_eq!(max["tools"].as_array().unwrap().len(), 9, "{max}");
    let ada = ada["id"].as_str().unwrap().to_owned();
    let ada_id: AgentId = ada.parse().unwrap();

    // A change reaches the connected runner with the tools.
    let mut ws = connect_runner(&d.agents, &backend.token("ada")).await;
    let tools = json!({"tools": ["team", "memory", "create_agents", "files", "shell"]});
    let (status, patched) = http(&d.ui, "PATCH", &format!("/v1/agents/{ada}"), Some(tools)).await;
    assert_eq!(status, 200, "{patched}");
    match next_frame(&mut ws).await {
        DaemonFrame::Configure { tools, .. } => {
            assert_eq!(tools, [Team, Memory, CreateAgents, Shell, Files]);
        }
        other => panic!("expected configure, got {other:?}"),
    }

    // Wider than the creator's own is refused before the operator is asked, also for connectors.
    let wider = AgentRequest {
        tools: Some(vec![Team, Web]),
        ..agent_request("scout")
    };
    let err = d.state.request_agent(ada_id, wider).await.unwrap_err();
    assert!(err.contains("you do not have: web"), "{err}");
    let foreign = AgentRequest {
        connectors: Some(vec!["github".into()]),
        ..agent_request("scout")
    };
    let err = d.state.request_agent(ada_id, foreign).await.unwrap_err();
    assert!(err.contains("github"), "{err}");
    assert!(
        !events(&d, &ada)
            .await
            .iter()
            .any(|e| e["kind"] == "question"),
        "no card"
    );

    // Narrower: the card lists the tools and the new agent gets exactly those.
    let st = d.state.clone();
    let narrow = AgentRequest {
        tools: Some(vec![Memory]),
        ..agent_request("scout")
    };
    let req = tokio::spawn(async move { st.request_agent(ada_id, narrow).await });
    let qid = approval_card(&d, &ada, 0).await;
    let card = events(&d, &ada)
        .await
        .into_iter()
        .find(|e| e["id"] == qid)
        .unwrap();
    let prompt = card["payload"]["card"]["questions"][0]["prompt"].clone();
    assert!(
        prompt.as_str().unwrap().contains("\nTools: memory"),
        "{prompt}"
    );
    assert_eq!(decide(&d, &ada, qid, "approve").await.0, 202);
    let AgentRequestOutcome::Created { id: scout, .. } = req.await.unwrap().unwrap() else {
        panic!("expected created");
    };
    let (_, view) = http(&d.ui, "GET", &format!("/v1/agents/{scout}"), None).await;
    assert_eq!(view["tools"], json!(["memory"]));

    // Auto mode skips the card, not the rule; omitted is the creator's own profile.
    let auto = json!({"auto_mode": true});
    assert_eq!(http(&d.ui, "PUT", "/v1/settings", Some(auto)).await.0, 200);
    let wider = AgentRequest {
        tools: Some(vec![Web]),
        ..agent_request("echo")
    };
    assert!(d.state.request_agent(ada_id, wider).await.is_err());
    let AgentRequestOutcome::Created { id: echo, .. } = d
        .state
        .request_agent(ada_id, agent_request("echo"))
        .await
        .unwrap()
    else {
        panic!("expected created");
    };
    let (_, view) = http(&d.ui, "GET", &format!("/v1/agents/{echo}"), None).await;
    assert_eq!(view["tools"], a_tools(&d, &ada).await);
    // Narrowing the creator later leaves its agents as they are.
    let narrower = json!({"tools": ["team"]});
    let path = format!("/v1/agents/{ada}");
    assert_eq!(http(&d.ui, "PATCH", &path, Some(narrower)).await.0, 200);
    let (_, view) = http(&d.ui, "GET", &format!("/v1/agents/{echo}"), None).await;
    assert_eq!(view["tools"].as_array().unwrap().len(), 5, "{view}");

    // A runner from before tool profiles still serves an agent with every built-in tool.
    let max = max["id"].as_str().unwrap().to_owned();
    let mut old = open_runner(&d.agents, &backend.token("max"), 1, false).await;
    assert!(matches!(
        next_frame(&mut old).await,
        DaemonFrame::Welcome { .. }
    ));
    // Turning one off replaces its computer instead of trusting the old runner with it.
    let no_shell = json!({"tools": ["team", "memory", "files", "web"]});
    let path = format!("/v1/agents/{max}");
    assert_eq!(http(&d.ui, "PATCH", &path, Some(no_shell)).await.0, 200);
    let calls = backend.calls();
    assert!(
        calls.contains(&"destroy max purge=false".to_owned()),
        "{calls:?}"
    );
    // An old runner on the new computer is refused, with a notice in the chat.
    let mut old = open_runner(&d.agents, &backend.token("max"), 1, false).await;
    let quiet = tokio::time::timeout(Duration::from_millis(300), old.next()).await;
    assert!(quiet.is_err(), "no welcome: {quiet:?}");
    let notice = eventually("refusal notice", Duration::from_secs(5), || async {
        events(&d, &max)
            .await
            .into_iter()
            .find(|e| e["kind"] == "runner_refused")
    })
    .await;
    assert!(
        notice["payload"]["reason"]
            .as_str()
            .unwrap()
            .contains("older runner"),
        "{notice}"
    );
    let (_, view) = http(&d.ui, "GET", &path, None).await;
    assert_eq!(view["connected"], false);
    // The current runner is welcome.
    connect_runner(&d.agents, &backend.token("max")).await;
}

async fn a_tools(d: &Daemon, id: &str) -> Value {
    http(&d.ui, "GET", &format!("/v1/agents/{id}"), None)
        .await
        .1["tools"]
        .clone()
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
    assert_eq!(card["payload"]["card"]["title"], "Approval needed");
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

    // A sub-agent's request shows up where the operator chats: its creator's chat.
    let st = d.state.clone();
    let req = tokio::spawn(async move { st.request_agent(id, agent_request("deep")).await });
    let q4 = approval_card(&d, &ada, q3).await;
    let card = events(&d, &ada)
        .await
        .into_iter()
        .find(|e| e["id"] == q4)
        .unwrap();
    assert_eq!(
        card["payload"]["card"]["title"],
        "scout wants to create an agent"
    );
    let (status, _) = decide(&d, &ada, q4, "decline").await;
    assert_eq!(status, 202);
    assert!(matches!(
        req.await.unwrap().unwrap(),
        AgentRequestOutcome::Declined { .. }
    ));
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

#[tokio::test]
async fn auto_mode_creates_agents_without_a_card() {
    use ferrobot_mcp::AgentRequestOutcome;
    async fn notices(d: &Daemon, agent: &str) -> Vec<Value> {
        let evs = events(d, agent).await;
        assert!(
            !evs.iter().any(|e| e["kind"] == "question"),
            "no card: {evs:?}"
        );
        evs.into_iter()
            .filter(|e| e["kind"] == "agent_created")
            .map(|e| e["payload"].clone())
            .collect()
    }
    let backend = Arc::new(StatefulBackend::default());
    let d = start_with(backend, None, "127.0.0.1:0", |s| s.max_agents = 3).await;
    let (status, body) = http(&d.ui, "GET", "/v1/settings", None).await;
    assert_eq!(
        (status, body),
        (200, json!({"auto_mode": false, "agent_names": null}))
    );
    let (status, body) = http(
        &d.ui,
        "PUT",
        "/v1/settings",
        Some(json!({"auto_mode": true})),
    )
    .await;
    assert_eq!(
        (status, body),
        (200, json!({"auto_mode": true, "agent_names": null}))
    );
    let (_, body) = http(&d.ui, "GET", "/v1/settings", None).await;
    assert_eq!(body["auto_mode"], true);

    let ada = create(&d, json!({"name": "ada"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let AgentRequestOutcome::Created { id: scout, .. } = d
        .state
        .request_agent(ada.parse().unwrap(), agent_request("scout"))
        .await
        .unwrap()
    else {
        panic!("expected created");
    };
    // A sub-agent's creation is noted where the operator chats: the top creator's chat.
    let AgentRequestOutcome::Created { id: deep, .. } = d
        .state
        .request_agent(scout, agent_request("deep"))
        .await
        .unwrap()
    else {
        panic!("expected created");
    };
    let expected = |name: &str, id: AgentId, by: &str| json!({"name": name, "id": id.to_string(), "by": by, "auto": true});
    assert_eq!(
        notices(&d, &ada).await,
        vec![
            expected("scout", scout, "ada"),
            expected("deep", deep, "scout")
        ]
    );

    let err = d
        .state
        .request_agent(scout, agent_request("echo"))
        .await
        .unwrap_err();
    assert!(err.contains("at most 3 agents"), "{err}");
    assert_eq!(
        notices(&d, &ada).await.len(),
        2,
        "nothing created over the limit"
    );
}

/// Fake upstream for any provider: echoes the method, path and credentials it saw, and
/// refuses keys containing `bad-key` with a 401 that quotes them.
async fn echo_upstream() -> String {
    async fn handler(request: axum::extract::Request) -> axum::response::Response {
        let (parts, body) = request.into_parts();
        let header = |name: &str| {
            parts
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        if let Some(bad) = [header("authorization"), header("x-api-key")]
            .into_iter()
            .flatten()
            .find(|v| v.contains("bad-key"))
        {
            let err = json!({"error": {"message": format!("invalid key {bad}")}});
            return (axum::http::StatusCode::UNAUTHORIZED, axum::Json(err)).into_response();
        }
        let body: Value = axum::body::to_bytes(body, usize::MAX)
            .await
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        axum::Json(json!({
            "method": parts.method.as_str(),
            "path": parts.uri.path(),
            "authorization": header("authorization"),
            "x_api_key": header("x-api-key"),
            "goog_key": header("x-goog-api-key"),
            "session": header("x-session-id"),
            "cache_control": body.get("cache_control"),
            "query": parts.uri.query(),
            "accounts": parts.headers.get_all("chatgpt-account-id").iter()
                .filter_map(|v| v.to_str().ok()).collect::<Vec<_>>(),
        }))
        .into_response()
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

/// Ends a fake runner's turn as failed with `error`.
async fn fail(ws: &mut Ws, turn_id: String, error: &str) {
    let done = RunnerFrame::TurnDone {
        turn_id,
        outcome: ferrobot_core::protocol::TurnOutcome::Failed,
        session_id: None,
        result: None,
        error: Some(error.into()),
        steered: vec![],
    };
    ws.send(Message::text(serde_json::to_string(&done).unwrap()))
        .await
        .unwrap();
}

#[tokio::test]
async fn credential_problems_hold_turns_until_fixed() {
    let upstream = echo_upstream().await;
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let id = create(&d, json!({"name": "kim"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut ws = connect_runner(&d.agents, &backend.token("kim")).await;
    let post = |body: &'static str| {
        let path = format!("/v1/agents/{id}/messages");
        let ui = d.ui.clone();
        async move {
            http(&ui, "POST", &path, Some(json!({"body": body})))
                .await
                .1["id"]
                .as_i64()
                .unwrap()
        }
    };
    let agent = || async {
        http(&d.ui, "GET", &format!("/v1/agents/{id}"), None)
            .await
            .1
    };
    let notices = || async {
        events(&d, &id)
            .await
            .into_iter()
            .filter(|e| e["kind"] == "credential_error")
            .collect::<Vec<_>>()
    };
    let (d, id) = (&d, id.as_str());
    let pending = |ids: Vec<i64>| async move {
        let mail = mail(d, id).await;
        ids.iter().all(|m| {
            mail.iter()
                .any(|x| x["id"] == *m && x["status"] == "pending")
        })
    };
    let call = |token: String| async move {
        let bearer = format!("Bearer {token}");
        model_call(d, "/model/chat/completions", ("authorization", &bearer)).await
    };
    let claude_call = || async {
        let token = backend.token("kim");
        model_call(d, "/model/v1/messages", ("x-api-key", &token)).await
    };

    // On the daemon default without a daemon key: the first turn fails once, then turns wait.
    let first = post("hello").await;
    let (turn, message, _) = next_turn(&mut ws).await;
    assert_eq!(message, first);
    assert_eq!(agent().await["credential_error"], Value::Null);
    let (status, _) = claude_call().await;
    assert_eq!(status, 400);
    let (status, _) = claude_call().await;
    assert_eq!(status, 400);
    fail(
        &mut ws,
        turn.clone(),
        "API Error: 400 the daemon has no Anthropic key",
    )
    .await;
    let problem = agent().await["credential_error"].clone();
    assert_eq!(problem["kind"], "missing", "{problem}");
    assert!(
        problem["reason"]
            .as_str()
            .unwrap()
            .contains("no Anthropic key")
    );
    let second = post("are you there?").await;
    let third = post("hello?").await;
    quiet(&mut ws).await;
    assert!(pending(vec![first, second, third]).await, "messages wait");
    let notes = notices().await;
    assert_eq!(notes.len(), 1, "one notice: {notes:?}");
    assert_eq!(notes[0]["turn_id"], Value::Null);
    assert_eq!(notes[0]["payload"]["turn_id"], json!(turn));

    // A provider with a key clears it, and the waiting messages run in order.
    let (status, provider) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(
            json!({"name": "oai", "kind": "openai", "base_url": format!("{upstream}/v1"),
                    "default_model": "gpt-test", "api_key": "good-key"}),
        ),
    )
    .await;
    assert_eq!(status, 201, "{provider}");
    let pid = provider["id"].as_str().unwrap().to_owned();
    let (status, view) = http(
        &d.ui,
        "PATCH",
        &format!("/v1/agents/{id}"),
        Some(json!({"provider_id": pid})),
    )
    .await;
    assert_eq!((status, &view["credential_error"]), (200, &Value::Null));
    let (turn, message, _) = next_turn(&mut ws).await;
    assert_eq!(message, first);
    assert_eq!(call(backend.token("kim")).await.0, 200);
    finish(&mut ws, turn, None).await;
    let (turn, message, _) = next_turn(&mut ws).await;
    assert_eq!(message, second);

    // A key the provider refuses: "rejected", without the key or the upstream's words.
    let set_key = |key: &'static str| {
        let path = format!("/v1/providers/{pid}");
        let ui = d.ui.clone();
        async move {
            let (status, _) = http(&ui, "PATCH", &path, Some(json!({"api_key": key}))).await;
            assert_eq!(status, 200);
        }
    };
    set_key("bad-key-123").await;
    assert_eq!(call(backend.token("kim")).await.0, 401);
    fail(&mut ws, turn, "API Error: 401 invalid key").await;
    let problem = agent().await["credential_error"].clone();
    assert_eq!(problem["kind"], "rejected", "{problem}");
    assert_eq!(
        problem["reason"],
        "provider oai rejected its API key: replace it in Settings, Vault"
    );
    quiet(&mut ws).await;
    assert!(pending(vec![second, third]).await);

    // A retry that did not fix it sets the problem again on the next call; the Claude SDK
    // ends such a turn as completed with the error as its answer, and the message still waits.
    let (status, view) = http(&d.ui, "POST", &format!("/v1/agents/{id}/retry"), None).await;
    assert_eq!((status, &view["credential_error"]), (200, &Value::Null));
    let (turn, message, _) = next_turn(&mut ws).await;
    assert_eq!(message, second);
    assert_eq!(call(backend.token("kim")).await.0, 401);
    finish(&mut ws, turn, Some("API Error: 401 invalid key")).await;
    assert_eq!(agent().await["credential_error"]["kind"], "rejected");
    quiet(&mut ws).await;
    assert!(
        pending(vec![second, third]).await,
        "a completed turn does not deliver it"
    );

    // Storing a good key (Vault saves through the provider) clears it.
    set_key("good-key").await;
    let (turn, message, _) = next_turn(&mut ws).await;
    assert_eq!(message, second);
    assert_eq!(agent().await["credential_error"], Value::Null);
    finish(&mut ws, turn, None).await;
    let (turn, message, _) = next_turn(&mut ws).await;
    assert_eq!(message, third);
    finish(&mut ws, turn, None).await;
    delivered(d, id, third).await;

    assert_eq!(notices().await.len(), 3, "one notice per problem");
    let everything = format!("{}{:?}", agent().await, events(d, id).await);
    assert!(!everything.contains("bad-key"), "the key never shows");
    assert!(!everything.contains("good-key"));
}

/// `OpenRouter`'s answer when no endpoint of the model supports tools.
const NO_TOOL_ENDPOINTS: &str = r#"{"error":{"message":"No endpoints found that support tool use. Try disabling \"bash\".","code":404}}"#;

/// Fake chat completions upstream: requests with tools get [`NO_TOOL_ENDPOINTS`] (model `tooled`
/// excepted), and so does every request for model `gone`; the rest get a completion.
async fn no_tools_upstream() -> String {
    async fn handler(body: axum::body::Bytes) -> axum::response::Response {
        let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let tools = req["tools"].as_array().is_some_and(|t| !t.is_empty());
        let model = req["model"].as_str().unwrap_or_default();
        if model == "gone" || (tools && model != "tooled") {
            let status = axum::http::StatusCode::NOT_FOUND;
            let json = [("content-type", "application/json")];
            return (status, json, NO_TOOL_ENDPOINTS).into_response();
        }
        axum::Json(json!({"choices": [{"message": {"role": "assistant", "content": "hi"}}]}))
            .into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().fallback(handler);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/v1")
}

#[tokio::test]
async fn models_without_tools_ask_once_and_hold_turns() {
    let upstream = no_tools_upstream().await;
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let backend = &*backend;
    let body = json!({"name": "or", "kind": "openrouter", "base_url": upstream,
                      "api_key": "or-key", "default_model": "dolphin"});
    let (status, provider) = http(&d.ui, "POST", "/v1/providers", Some(body)).await;
    assert_eq!(status, 201, "{provider}");
    let id = create(&d, json!({"name": "kim", "provider_id": provider["id"]})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (d, id) = (&d, id.as_str());
    let conn = add_connector(d, json!({"name": "open", "url": keyless_upstream().await})).await;
    let assign = format!("/v1/agents/{id}/connectors");
    let (status, _) = http(&d.ui, "PUT", &assign, Some(json!([conn["id"]]))).await;
    assert_eq!(status, 200);
    let mut ws = connect_runner(&d.agents, &backend.token("kim")).await;
    let post = |body: &'static str| async move {
        let path = format!("/v1/agents/{id}/messages");
        http(&d.ui, "POST", &path, Some(json!({"body": body})))
            .await
            .1["id"]
            .as_i64()
            .unwrap()
    };
    let agent = || async {
        http(&d.ui, "GET", &format!("/v1/agents/{id}"), None)
            .await
            .1
    };
    let patch = |body: Value| async move {
        let (status, view) = http(&d.ui, "PATCH", &format!("/v1/agents/{id}"), Some(body)).await;
        assert_eq!(status, 200, "{view}");
        view
    };
    let cards = || async {
        events(d, id)
            .await
            .into_iter()
            .filter(|e| e["kind"] == "question")
            .filter(|e| e["payload"]["approval"]["kind"] == "use_without_tools")
            .collect::<Vec<_>>()
    };
    let pending = |ids: Vec<i64>| async move {
        let mail = mail(d, id).await;
        ids.iter().all(|m| {
            mail.iter()
                .any(|x| x["id"] == *m && x["status"] == "pending")
        })
    };
    // The runner's model call, with a built-in tool or with an empty list.
    let call = |tools: bool| async move {
        let bearer = format!("Bearer {}", backend.token("kim"));
        let tools = if tools {
            json!([{"type": "function", "function": {"name": "bash"}}])
        } else {
            json!([])
        };
        let model = agent().await["model"]
            .as_str()
            .unwrap_or("dolphin")
            .to_owned();
        let body = json!({"model": model, "messages": [], "tools": tools});
        let headers = [
            ("authorization", bearer.as_str()),
            ("content-type", "application/json"),
        ];
        let url = "/model/chat/completions";
        let out = http_raw(
            &d.agents,
            "POST",
            url,
            &headers,
            body.to_string().as_bytes(),
        )
        .await;
        (out.0, String::from_utf8(out.2).unwrap())
    };
    let failed_turn = |mut ws: Ws, message: i64| async move {
        let (turn, got, _) = next_turn(&mut ws).await;
        assert_eq!(got, message);
        let (status, text) = call(true).await;
        assert_eq!(status, 404);
        fail(
            &mut ws,
            turn,
            &format!("model request failed with 404: {text}"),
        )
        .await;
        ws
    };

    // The first refusal holds the turns and asks once; the message waits instead of failing.
    let first = post("hello").await;
    ws = failed_turn(ws, first).await;
    let problem = agent().await["tools_unsupported"].clone();
    assert_eq!(problem["model"], "dolphin", "{problem}");
    assert_eq!(problem["reason"], "or can't run dolphin with tools");
    assert_eq!(agent().await["credential_error"], Value::Null);
    let found = cards().await;
    assert_eq!(found.len(), 1, "one card: {found:?}");
    let card = &found[0]["payload"]["card"];
    assert_eq!(card["title"], "kim's model can't use tools");
    let prompt = card["questions"][0]["prompt"].as_str().unwrap();
    assert!(prompt.contains("only chat: no shell, files"), "{prompt}");
    let labels: Vec<&Value> = card["questions"][0]["choices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["label"])
        .collect();
    assert_eq!(labels, [&json!("Use without tools"), &json!("Keep tools")]);
    let notices = events(d, id).await;
    assert!(notices.iter().any(|e| e["kind"] == "tools_unsupported"));
    quiet(&mut ws).await;
    assert!(pending(vec![first]).await, "not failed");

    // Declined: nothing changes, later messages wait too and no second card comes.
    let qid = found[0]["id"].as_i64().unwrap();
    assert_eq!(decide(d, id, qid, "decline").await.0, 202);
    let second = post("are you there?").await;
    quiet(&mut ws).await;
    assert!(pending(vec![first, second]).await);
    assert_eq!(cards().await.len(), 1);
    assert_ne!(agent().await["tools_unsupported"], Value::Null);

    // A new model clears the hold; refused again, a new card comes, but a retry with that card
    // still open adds none.
    let view = patch(json!({"model": "dolphin-2"})).await;
    assert_eq!(view["tools_unsupported"], Value::Null);
    ws = failed_turn(ws, first).await;
    assert_eq!(cards().await.len(), 2);
    let (status, view) = http(&d.ui, "POST", &format!("/v1/agents/{id}/retry"), None).await;
    assert_eq!((status, &view["tools_unsupported"]), (200, &Value::Null));
    ws = failed_turn(ws, first).await;
    let found = cards().await;
    assert_eq!(found.len(), 2, "one open card at most");

    // Approved: no tools and no connectors, and the waiting messages run without tools.
    let qid = found[1]["id"].as_i64().unwrap();
    assert_eq!(decide(d, id, qid, "approve").await.0, 202);
    match next_frame(&mut ws).await {
        DaemonFrame::Configure { tools, .. } => assert!(tools.is_empty(), "{tools:?}"),
        other => panic!("expected configure, got {other:?}"),
    }
    let (turn, got, _) = next_turn(&mut ws).await;
    assert_eq!(got, first);
    assert_eq!(call(false).await.0, 200);
    finish(&mut ws, turn, Some("hi")).await;
    let (turn, got, _) = next_turn(&mut ws).await;
    assert_eq!(got, second);
    finish(&mut ws, turn, None).await;
    delivered(d, id, second).await;
    assert_eq!(a_tools(d, id).await, json!([]));
    assert_eq!(http(&d.ui, "GET", &assign, None).await.1, json!([]));
    assert_eq!(agent().await["tools_unsupported"], Value::Null);

    // A request without tools is never taken for a tools problem, whatever the upstream says.
    patch(json!({"model": "gone"})).await;
    let (status, text) = call(false).await;
    assert_eq!(status, 404);
    assert!(text.contains("support tool use"), "the body passes through");
    assert_eq!(agent().await["tools_unsupported"], Value::Null);
    assert_eq!(cards().await.len(), 2);

    // A card left open after the model changed does not take the tools of an agent that works.
    patch(json!({"tools": ["shell"]})).await;
    assert!(matches!(
        next_frame(&mut ws).await,
        DaemonFrame::Configure { .. }
    ));
    let third = post("still there?").await;
    ws = failed_turn(ws, third).await;
    let found = cards().await;
    assert_eq!(found.len(), 3);
    patch(json!({"model": "tooled"})).await;
    let (turn, got, _) = next_turn(&mut ws).await;
    assert_eq!(got, third);
    assert_eq!(call(true).await.0, 200);
    finish(&mut ws, turn, None).await;
    delivered(d, id, third).await;
    let qid = found[2]["id"].as_i64().unwrap();
    assert_eq!(decide(d, id, qid, "approve").await.0, 202);
    quiet(&mut ws).await;
    assert_eq!(a_tools(d, id).await, json!(["shell"]));
}

#[tokio::test]
async fn openrouter_calls_cache_claude_and_stick_to_one_provider() {
    let upstream = echo_upstream().await;
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let mut ids = Vec::new();
    for (name, kind) in [("or", "openrouter"), ("oai", "openai")] {
        let body = json!({"name": name, "kind": kind, "base_url": format!("{upstream}/v1"),
                          "api_key": "k", "default_model": "m"});
        let (status, p) = http(&d.ui, "POST", "/v1/providers", Some(body)).await;
        assert_eq!(status, 201, "{p}");
        ids.push(p["id"].clone());
    }
    let ann = create(&d, json!({"name": "ann", "provider_id": ids[0]})).await;
    let bob = create(&d, json!({"name": "bob", "provider_id": ids[1]})).await;
    let call = |agent: &'static str, model: &str| {
        let bearer = format!("Bearer {}", backend.token(agent));
        let body = json!({"model": model, "messages": []}).to_string();
        let ui = d.agents.clone();
        async move {
            let (_, _, out) = http_raw(
                &ui,
                "POST",
                "/model/chat/completions",
                &[
                    ("authorization", bearer.as_str()),
                    ("content-type", "application/json"),
                ],
                body.as_bytes(),
            )
            .await;
            serde_json::from_slice::<Value>(&out).unwrap()
        }
    };

    let seen = call("ann", "anthropic/claude-sonnet-5.5").await;
    assert_eq!(seen["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!(seen["session"], ann["id"], "one session per agent");
    let seen = call("ann", "openai/gpt-6").await;
    assert_eq!(
        seen["cache_control"],
        Value::Null,
        "other models cache on their own"
    );
    assert_eq!(seen["session"], ann["id"]);
    let seen = call("bob", "anthropic/claude-sonnet-5.5").await;
    assert_eq!(
        (&seen["cache_control"], &seen["session"]),
        (&Value::Null, &Value::Null),
        "only OpenRouter"
    );
    let _ = bob;
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
    assert_eq!(status, 400, "{err}");

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
    // Without its own key the daemon cannot serve an agent without a provider.
    let (status, err) = http(
        &d.ui,
        "PATCH",
        &format!("/v1/agents/{olly_id}"),
        Some(json!({"provider_id": null})),
    )
    .await;
    assert_eq!(status, 400, "{err}");
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("no Anthropic key"),
        "{err}"
    );
    let (_, health) = http(&d.ui, "GET", "/v1/health", None).await;
    assert_eq!(health["anthropic_key"], false);
    let (status, _) = http(&d.ui, "DELETE", &format!("/v1/agents/{olly_id}"), None).await;
    assert_eq!(status, 204);
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
AUTH="$CODEX_HOME/auth.json"
ls -ld "$CODEX_HOME" | cut -c1-10 >> "$(dirname "$0")/seen.log"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{"id":%s,"result":{}}\n' "$id" ;;
    *'"getAuthStatus"'*)
      if [ -f "$AUTH" ]; then
        ls -l "$AUTH" | cut -c1-10 >> "$(dirname "$0")/seen.log"
        case "$line" in *'"refreshToken":true'*) printf '{"tokens":"refreshed"}' > "$AUTH" ;; esac
        printf '{"id":%s,"result":{"authMethod":"chatgpt","authToken":"%s"}}\n' "$id" "$JWT"
      else
        printf '{"id":%s,"result":{"authMethod":null}}\n' "$id"
      fi ;;
    *'"account/rateLimits/read"'*)
      printf '{"id":%s,"result":{"rateLimits":{"primary":{"usedPercent":12,"windowDurationMins":300,"resetsAt":4102444800},"secondary":{"usedPercent":40,"windowDurationMins":10080,"resetsAt":null}}}}\n' "$id" ;;
    *'"account/login/start"'*)
      printf '{"id":%s,"result":{"type":"chatgptDeviceCode","loginId":"l1","verificationUrl":"https://auth.example/device","userCode":"ABCD-1234"}}\n' "$id"
      printf '{"tokens":"signed-in"}' > "$AUTH"
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
    let (status, limits) = http(&d.ui, "GET", "/v1/limits", None).await;
    assert_eq!(status, 200, "{limits}");
    assert_eq!(
        limits.as_array().unwrap().len(),
        1,
        "not the keyed one: {limits}"
    );
    assert_eq!(limits[0]["provider_id"], chat["id"]);
    assert_eq!(limits[0]["five_hour"]["used_percent"], 12.0);
    assert_eq!(limits[0]["five_hour"]["resets"], "2100-01-01T00:00:00Z");
    assert_eq!(limits[0]["weekly"]["used_percent"], 40.0);
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

    // Codex's auth.json lives in the secret store; on disk only while Codex runs.
    let chat_id: uuid::Uuid = chat["id"].as_str().unwrap().parse().unwrap();
    let key = ferrobot_daemon::codex::secret_key(chat_id);
    let secrets = &d.state.settings.secrets;
    assert_eq!(
        secrets.get(&key).await.unwrap().as_deref(),
        Some(r#"{"tokens":"refreshed"}"#),
        "the refreshed sign-in is kept"
    );
    let auth_files = || {
        fn walk(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                if entry.path().is_dir() {
                    walk(&entry.path(), found);
                } else if entry.file_name() == "auth.json" {
                    found.push(entry.path());
                }
            }
        }
        let mut found = Vec::new();
        walk(&dir.path().join("homes"), &mut found);
        found
    };
    eventually("no auth.json left", Duration::from_secs(5), || async {
        auth_files().is_empty().then_some(())
    })
    .await;
    let seen = std::fs::read_to_string(dir.path().join("seen.log")).unwrap();
    assert!(
        seen.lines().all(|l| l == "drwx------" || l == "-rw-------"),
        "{seen}"
    );
    assert!(seen.contains("-rw-------"), "{seen}");

    // A sign-in from before the secret store moves over on first use.
    let (_, old) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(json!({"name": "cx-old", "kind": "codex", "default_model": "gpt-5-codex"})),
    )
    .await;
    let old_id: uuid::Uuid = old["id"].as_str().unwrap().parse().unwrap();
    let legacy = dir.path().join("homes").join(old_id.to_string());
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("auth.json"), r#"{"tokens":"old"}"#).unwrap();
    let (_, s) = http(
        &d.ui,
        "GET",
        &format!("/v1/providers/{old_id}/chatgpt-login"),
        None,
    )
    .await;
    assert_eq!(s["state"], "signed_in");
    assert!(!legacy.exists());
    let old_key = ferrobot_daemon::codex::secret_key(old_id);
    assert!(secrets.get(&old_key).await.unwrap().is_some());

    // Deleting the provider deletes its sign-in.
    let (status, _) = http(&d.ui, "DELETE", &format!("/v1/providers/{old_id}"), None).await;
    assert_eq!(status, 204);
    assert!(secrets.get(&old_key).await.unwrap().is_none());
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

#[tokio::test]
async fn backends_without_pause_stay_awake_until_the_cold_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let local = Arc::new(
        ferrobot_sandbox::LocalProcessBackend::new(ferrobot_sandbox::LocalProcessConfig {
            root: tmp.path().to_path_buf(),
            runner_cmd: vec!["sleep".into(), "30".into()],
        })
        .unwrap(),
    );
    let d = start_with(local, None, "127.0.0.1:0", |s| {
        s.warm_timeout = Some(Duration::from_millis(100));
        s.idle_timeout = Some(Duration::from_secs(60));
    })
    .await;
    let id = create(&d, json!({"name": "lou"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    tokio::time::sleep(Duration::from_millis(200)).await;
    ferrobot_daemon::supervisor::check_all(&d.state).await;
    let (_, a) = http(&d.ui, "GET", &format!("/v1/agents/{id}"), None).await;
    assert_eq!(a["status"], "running", "no pause on the local backend: {a}");
    let (status, _) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{id}/stop?level=warm"),
        None,
    )
    .await;
    assert_eq!(status, 501);
    http(&d.ui, "DELETE", &format!("/v1/agents/{id}"), None).await;
}

#[tokio::test]
async fn gemini_cli_agents_reach_the_gemini_api_through_the_proxy() {
    let upstream = echo_upstream().await;
    let backend = Arc::new(StatefulBackend::default());
    let d = start_with(backend.clone(), None, "127.0.0.1:0", |s| {
        s.driver = "claude".into();
    })
    .await;
    let (status, provider) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(
            json!({"name": "gem", "kind": "gemini_cli", "base_url": upstream,
                    "api_key": "AIza-real-gemini-key", "default_model": "gemini-2.5-flash"}),
        ),
    )
    .await;
    assert_eq!(status, 201, "{provider}");
    create(&d, json!({"name": "gina", "provider_id": provider["id"]})).await;
    let env = backend.envs.lock().unwrap()["gina"].clone();
    let token = env["FERROBOT_AGENT_TOKEN"].clone();
    assert_eq!(env["FERROBOT_DRIVER"], "acp");
    assert_eq!(env["FERROBOT_ACP_AGENT"], "gemini");
    assert_eq!(env["GEMINI_API_KEY"], token, "only the agent's own token");
    assert!(env["GOOGLE_GEMINI_BASE_URL"].ends_with("/model"));
    assert!(!env.values().any(|v| v.contains("AIza-real-gemini-key")));
    let (status, seen) = model_call(
        &d,
        "/model/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse",
        ("x-goog-api-key", &token),
    )
    .await;
    assert_eq!(status, 200, "{seen}");
    assert_eq!(
        seen["path"],
        "/v1beta/models/gemini-2.5-flash:streamGenerateContent"
    );
    assert_eq!(seen["query"], "alt=sse");
    assert_eq!(seen["goog_key"], "AIza-real-gemini-key");
    let (status, _) = model_call(&d, "/model/v1beta/models", ("x-goog-api-key", "wrong")).await;
    assert_eq!(status, 401);
}

/// Fake model lists that answer only when the provider's key arrives in the right header.
async fn models_upstream() -> String {
    async fn handler(request: axum::extract::Request) -> axum::response::Response {
        let header = |name: &str| {
            request
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        let uri = request.uri();
        let body = match uri.path() {
            "/v1/models"
                if header("x-api-key") == "sk-ant-secret"
                    && !header("anthropic-version").is_empty()
                    && uri.query() == Some("limit=1000") =>
            {
                json!({"data": [{"id": "claude-x", "display_name": "Claude X"}], "has_more": false})
            }
            "/oai/models" if header("authorization") == "Bearer sk-oai-secret" => {
                json!({"object": "list", "data": [{"id": "gpt-x", "object": "model"}]})
            }
            _ => {
                return (
                    axum::http::StatusCode::UNAUTHORIZED,
                    "Incorrect API key provided: sk-wrong-secret",
                )
                    .into_response();
            }
        };
        axum::Json(body).into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().fallback(handler))
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

async fn model_list(d: &Daemon, provider: &str) -> Value {
    let (status, list) = http(
        &d.ui,
        "GET",
        &format!("/v1/providers/{provider}/models"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{list}");
    assert!(!list.to_string().contains("secret"), "a key leaked: {list}");
    list
}

#[tokio::test]
async fn model_lists_come_from_the_provider_or_suggestions() {
    let upstream = models_upstream().await;
    let closed = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let mut ids = HashMap::new();
    for (name, kind, base_url, key) in [
        ("ant", "anthropic", upstream.clone(), "sk-ant-secret"),
        ("oai", "openai", format!("{upstream}/oai"), "sk-oai-secret"),
        (
            "wrong",
            "openai",
            format!("{upstream}/oai"),
            "sk-wrong-secret",
        ),
        ("down", "openai", closed.clone(), "sk-oai-secret"),
    ] {
        let (status, p) = http(
            &d.ui,
            "POST",
            "/v1/providers",
            Some(json!({"name": name, "kind": kind, "base_url": base_url, "api_key": key})),
        )
        .await;
        assert_eq!(status, 201, "{p}");
        ids.insert(name, p["id"].as_str().unwrap().to_owned());
    }

    assert_eq!(
        model_list(&d, &ids["ant"]).await,
        json!({"source": "provider", "models": [{"id": "claude-x", "name": "Claude X"}]})
    );
    assert_eq!(
        model_list(&d, &ids["oai"]).await,
        json!({"source": "provider", "models": [{"id": "gpt-x", "name": null}]})
    );
    for name in ["wrong", "down"] {
        let list = model_list(&d, &ids[name]).await;
        assert_eq!(list["source"], "suggested", "{name}: {list}");
        assert!(
            list["models"][0]["id"]
                .as_str()
                .unwrap()
                .starts_with("gpt-")
        );
    }

    // A fixed key or URL shows at once: failures are not cached and edits drop the cache.
    for (name, body) in [
        ("wrong", json!({"api_key": "sk-oai-secret"})),
        ("ant", json!({"base_url": closed})),
    ] {
        let path = format!("/v1/providers/{}", ids[name]);
        assert_eq!(http(&d.ui, "PATCH", &path, Some(body)).await.0, 200);
    }
    assert_eq!(model_list(&d, &ids["wrong"]).await["source"], "provider");
    assert_eq!(model_list(&d, &ids["ant"]).await["source"], "suggested");

    let (status, list) = http(&d.ui, "GET", "/v1/provider-kinds/bedrock/models", None).await;
    assert_eq!(status, 200);
    assert_eq!(list["source"], "suggested");
    assert!(
        list["models"][0]["id"]
            .as_str()
            .unwrap()
            .contains("anthropic.claude")
    );
    let (status, _) = http(&d.ui, "GET", "/v1/provider-kinds/grok/models", None).await;
    assert_eq!(status, 404);
}

async fn mcp_status(agents: &str, token: &str) -> u16 {
    let bearer = format!("Bearer {token}");
    let headers = [
        ("Authorization", bearer.as_str()),
        ("Content-Type", "application/json"),
    ];
    http_raw(agents, "POST", "/mcp", &headers, b"{}").await.0
}

#[tokio::test]
async fn idle_sleeping_agents_are_archived_and_restored_on_wake() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start_with(backend.clone(), None, "127.0.0.1:0", |s| {
        s.idle_timeout = Some(Duration::from_millis(100));
        s.archive_timeout = Some(Duration::from_millis(400));
    })
    .await;
    let id = create(&d, json!({"name": "arlo"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let old_token = backend.token("arlo");
    assert_ne!(mcp_status(&d.agents, &old_token).await, 401);
    let status = || async {
        http(&d.ui, "GET", &format!("/v1/agents/{id}"), None)
            .await
            .1
    };

    tokio::time::sleep(Duration::from_millis(200)).await;
    ferrobot_daemon::supervisor::check_all(&d.state).await;
    assert_eq!(status().await["status"], "stopped");
    tokio::time::sleep(Duration::from_millis(300)).await;
    ferrobot_daemon::supervisor::check_all(&d.state).await;
    let a = status().await;
    assert_eq!(
        (&a["status"], &a["presence"]),
        (&json!("archived"), &json!("sleeping"))
    );
    assert!(backend.calls().contains(&"archive arlo".to_owned()));
    assert!(
        d.state
            .store
            .agent_by_token(&old_token)
            .await
            .unwrap()
            .is_none(),
        "the token inside the archived image is retired"
    );
    assert_eq!(
        mcp_status(&d.agents, &old_token).await,
        401,
        "the archived token no longer opens /mcp"
    );

    // A message brings it back from the archive with a fresh token.
    let (status_code, _) = http(
        &d.ui,
        "POST",
        &format!("/v1/agents/{id}/messages"),
        Some(json!({"body": "back"})),
    )
    .await;
    assert_eq!(status_code, 202);
    eventually("unarchive", Duration::from_secs(5), || async {
        (status().await["status"] == "running").then_some(())
    })
    .await;
    assert!(backend.calls().contains(&"unarchive arlo".to_owned()));
    assert_ne!(backend.token("arlo"), old_token);

    http(&d.ui, "DELETE", &format!("/v1/agents/{id}"), None).await;
    assert!(backend.calls().contains(&"delete_archive arlo".to_owned()));
}

struct Quiet;

impl MailboxNotifier for Quiet {
    fn message_enqueued(&self, _: &ferrobot_core::MailMessage) {}
}

/// Serves ferrobot's own MCP server as a connector upstream that only accepts
/// `Bearer secret`. Returns its URL and every `Authorization` header it saw.
async fn upstream_mcp() -> (String, Arc<Mutex<Vec<String>>>) {
    upstream_with("secret", |_| axum::Router::new()).await
}

/// ferrobot's own MCP server at `/mcp`, accepting only `Bearer <token>`.
async fn mcp_upstream_app(token: &str) -> axum::Router {
    let store = ferrobot_store::Store::open_in_memory().unwrap();
    let up = store
        .create_agent(ferrobot_store::NewAgent {
            created_by: None,
            name: ferrobot_core::AgentName::parse("up").unwrap(),
            description: String::new(),
            instructions: String::new(),
        })
        .await
        .unwrap();
    store.set_agent_token(up.id, token).await.unwrap();
    axum::Router::new().nest(
        "/mcp",
        ferrobot_mcp::router(ferrobot_mcp::McpState {
            store,
            notifier: Arc::new(Quiet),
        }),
    )
}

/// [`upstream_mcp`] that needs no token: it adds the one it accepts itself.
async fn keyless_upstream() -> String {
    serve_upstream(keyless_app().await).await
}

/// [`keyless_upstream`] that refuses an `initialize` offering protocol 2026-07-28 with "Method
/// not supported" instead of offering an older version, as Google's MCP servers do.
async fn legacy_upstream() -> String {
    let app = keyless_app().await.layer(axum::middleware::from_fn(
        |req: axum::extract::Request, next: axum::middleware::Next| async move {
            let (parts, body) = req.into_parts();
            let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
            let rpc: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            if rpc["method"] == "initialize" && rpc["params"]["protocolVersion"] == "2026-07-28" {
                let error = json!({"jsonrpc": "2.0", "id": rpc["id"],
                    "error": {"code": -32601, "message": "Method not supported"}});
                return axum::response::IntoResponse::into_response(axum::Json(error));
            }
            let req = axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes));
            next.run(req).await
        },
    ));
    serve_upstream(app).await
}

async fn keyless_app() -> axum::Router {
    mcp_upstream_app("open")
        .await
        .layer(axum::middleware::from_fn(
            |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
                let open = axum::http::HeaderValue::from_static("Bearer open");
                req.headers_mut().insert("authorization", open);
                next.run(req).await
            },
        ))
}

async fn serve_upstream(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

/// [`upstream_mcp`] accepting `token`, plus `extra` routes on the same origin.
async fn upstream_with(
    token: &str,
    extra: impl FnOnce(&str) -> axum::Router,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    let app = mcp_upstream_app(token)
        .await
        .merge(extra(&base))
        .layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let record = record.clone();
                async move {
                    if let Some(v) = req.headers().get("authorization") {
                        record.lock().unwrap().push(v.to_str().unwrap().to_owned());
                    }
                    next.run(req).await
                }
            },
        ));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("{base}/mcp"), seen)
}

/// A minimal OAuth server for `<base>/mcp`: metadata, dynamic registration and a token
/// endpoint that checks PKCE against the challenge the test read from the authorization URL.
fn fake_auth_server(base: &str, challenge: Arc<Mutex<Option<String>>>) -> axum::Router {
    use axum::routing::{get, post};
    use base64::Engine as _;
    use sha2::Digest as _;
    let json = |v: Value| {
        move || {
            let v = v.clone();
            async move { axum::Json(v) }
        }
    };
    let prm = json!({"resource": format!("{base}/mcp"), "authorization_servers": [base]});
    let meta = json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
    });
    let token = move |body: String| {
        let challenge = challenge.clone();
        async move {
            let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
                .into_owned()
                .collect();
            let verifier = form.get("code_verifier").cloned().unwrap_or_default();
            let derived = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(sha2::Sha256::digest(verifier.as_bytes()));
            let expected = challenge.lock().unwrap().clone();
            if form.get("code").map(String::as_str) != Some("code-1")
                || expected.as_deref() != Some(derived.as_str())
            {
                let err = json!({"error": "invalid_grant"});
                return (axum::http::StatusCode::BAD_REQUEST, axum::Json(err));
            }
            let tokens = json!({"access_token": "at-1", "token_type": "Bearer",
                                "expires_in": 3600, "refresh_token": "rt-1"});
            (axum::http::StatusCode::OK, axum::Json(tokens))
        }
    };
    axum::Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(json(prm.clone())),
        )
        .route("/.well-known/oauth-protected-resource/mcp", get(json(prm)))
        .route("/.well-known/oauth-authorization-server", get(json(meta)))
        .route(
            "/register",
            post(|body: axum::Json<Value>| async move {
                let client = json!({"client_id": "client-1", "redirect_uris": body["redirect_uris"],
                                    "token_endpoint_auth_method": "none"});
                (axum::http::StatusCode::CREATED, axum::Json(client))
            }),
        )
        .route("/token", post(token))
}

async fn mcp_as(d: &Daemon, token: String) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    use rmcp::ServiceExt;
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    let config = StreamableHttpClientTransportConfig::with_uri(format!("http://{}/mcp", d.agents))
        .auth_header(token);
    ().serve(StreamableHttpClientTransport::from_config(config))
        .await
        .unwrap()
}

#[tokio::test]
async fn connectors_lend_upstream_tools_to_assigned_agents_only() {
    use rmcp::model::CallToolRequestParams;
    let (url, seen) = upstream_mcp().await;
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let ann = create(&d, json!({"name": "ann"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    create(&d, json!({"name": "ben"})).await;

    let body = json!({"name": "up", "url": url, "token": "secret"});
    let (status, conn) = http(&d.ui, "POST", "/v1/connectors", Some(body)).await;
    assert_eq!(status, 201, "{conn}");
    assert_eq!(conn["has_token"], true);
    let bad = json!({"name": "Up!", "url": url});
    let (status, _) = http(&d.ui, "POST", "/v1/connectors", Some(bad)).await;
    assert_eq!(status, 400);
    let (_, all) = http(&d.ui, "GET", "/v1/connectors", None).await;
    assert!(!all.to_string().contains("\"secret\""), "{all}");
    let assign = format!("/v1/agents/{ann}/connectors");
    let mut runner = connect_runner(&d.agents, &backend.token("ann")).await;
    let (status, ids) = http(&d.ui, "PUT", &assign, Some(json!([conn["id"]]))).await;
    assert_eq!((status, &ids), (200, &json!([conn["id"]])));
    // Runners whose model lists tools once per process start a fresh one.
    assert!(matches!(
        next_frame(&mut runner).await,
        DaemonFrame::Configure { .. }
    ));
    let (status, _) = http(&d.ui, "PUT", &assign, Some(json!([AgentId::new()]))).await;
    assert_eq!(status, 404, "unknown connector");

    // ann sees and calls the upstream's tools, authenticated with the token.
    let as_ann = mcp_as(&d, backend.token("ann")).await;
    let tools = as_ann.list_all_tools().await.unwrap();
    assert!(
        tools.iter().any(|t| t.name == "up__list_agents"),
        "{tools:?}"
    );
    assert!(tools.iter().any(|t| t.name == "send_message"));
    let r = as_ann
        .call_tool(CallToolRequestParams::new("up__list_agents"))
        .await
        .unwrap();
    let listed = r.structured_content.unwrap();
    assert_eq!(listed["agents"][0]["name"], "up", "{listed}");
    let headers = seen.lock().unwrap().clone();
    assert_ne!(headers, Vec::<String>::new());
    assert!(headers.iter().all(|h| h == "Bearer secret"), "{headers:?}");

    // ben was not given the connector: not listed, and calling it fails.
    let as_ben = mcp_as(&d, backend.token("ben")).await;
    let tools = as_ben.list_all_tools().await.unwrap();
    assert!(!tools.iter().any(|t| t.name.starts_with("up__")));
    assert!(
        as_ben
            .call_tool(CallToolRequestParams::new("up__list_agents"))
            .await
            .is_err()
    );

    // Agents created by ann inherit its connectors.
    let spec = ferrobot_daemon::agents::AgentSpec {
        name: "cal".into(),
        description: String::new(),
        instructions: String::new(),
        character: None,
        provider_id: None,
        model: None,
        tools: None,
        connectors: None,
        tags: Vec::new(),
    };
    let cal = ferrobot_daemon::agents::create(&d.state, spec, Some(ann.parse().unwrap()))
        .await
        .unwrap();
    let path = format!("/v1/agents/{}/connectors", cal.id);
    let (_, ids) = http(&d.ui, "GET", &path, None).await;
    assert_eq!(ids, json!([conn["id"]]));

    // Deleting removes the token and the tools.
    let id = conn["id"].as_str().unwrap();
    let (status, _) = http(&d.ui, "DELETE", &format!("/v1/connectors/{id}"), None).await;
    assert_eq!(status, 204);
    let key = ferrobot_daemon::connectors::secret_key(id.parse().unwrap());
    assert_eq!(d.state.settings.secrets.get(&key).await.unwrap(), None);
    let tools = as_ann.list_all_tools().await.unwrap();
    assert!(!tools.iter().any(|t| t.name.starts_with("up__")));
    as_ann.cancel().await.unwrap();
    as_ben.cancel().await.unwrap();
}

#[tokio::test]
async fn connectors_sign_in_with_oauth() {
    use rmcp::model::CallToolRequestParams;
    let challenge = Arc::new(Mutex::new(None));
    let as_challenge = challenge.clone();
    let (url, seen) = upstream_with("at-1", move |base| fake_auth_server(base, as_challenge)).await;
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let ann = create(&d, json!({"name": "ann"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_, conn) = http(
        &d.ui,
        "POST",
        "/v1/connectors",
        Some(json!({"name": "up", "url": url})),
    )
    .await;
    assert_eq!(
        (conn["has_token"].clone(), conn["signed_in"].clone()),
        (json!(false), json!(false))
    );
    let id = conn["id"].as_str().unwrap().to_owned();
    http(
        &d.ui,
        "PUT",
        &format!("/v1/agents/{ann}/connectors"),
        Some(json!([id])),
    )
    .await;

    // Start: discovery, registration, PKCE. The URL goes to the browser.
    let (status, started) = http(&d.ui, "POST", &format!("/v1/connectors/{id}/oauth"), None).await;
    assert_eq!(status, 200, "{started}");
    let auth = url::Url::parse(started["authorization_url"].as_str().unwrap()).unwrap();
    let q: HashMap<String, String> = auth.query_pairs().into_owned().collect();
    assert!(
        auth.as_str()
            .starts_with(&url.replace("/mcp", "/authorize")),
        "{auth}"
    );
    assert_eq!(q["client_id"], "client-1");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(
        q["redirect_uri"],
        format!("http://{}/v1/oauth/callback", d.ui)
    );
    *challenge.lock().unwrap() = Some(q["code_challenge"].clone());
    let csrf = &q["state"];

    // The browser comes back without the operator token.
    let no_token = [("Authorization", "")];
    let back = format!("/v1/oauth/callback?code=code-1&state={csrf}");
    let (status, headers, page) = http_raw(&d.ui, "GET", &back, &no_token, b"").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&page));
    assert!(
        headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-security-policy")
                && v.starts_with("default-src 'none'"))
    );
    let (status, _, _) = http_raw(&d.ui, "GET", &back, &no_token, b"").await;
    assert_eq!(status, 400, "a state works once");
    let forged = "/v1/oauth/callback?code=code-1&state=guessed";
    let (status, _, page) = http_raw(&d.ui, "GET", forged, &no_token, b"").await;
    assert_eq!(status, 400);
    assert!(
        !String::from_utf8_lossy(&page).contains("guessed"),
        "nothing is echoed"
    );

    let (_, all) = http(&d.ui, "GET", "/v1/connectors", None).await;
    assert_eq!(all[0]["signed_in"], true);
    assert!(!all.to_string().contains("at-1") && !all.to_string().contains("rt-1"));
    // The sign-in is checked on its own, with the new token.
    let check = eventually(
        "the check after signing in",
        Duration::from_secs(10),
        || async {
            let (_, all) = http(&d.ui, "GET", "/v1/connectors", None).await;
            (!all[0]["check"].is_null()).then(|| all[0]["check"].clone())
        },
    )
    .await;
    assert_eq!(check["status"], "ok", "{check}");

    // ann uses the upstream with the OAuth access token.
    let as_ann = mcp_as(&d, backend.token("ann")).await;
    let r = as_ann
        .call_tool(CallToolRequestParams::new("up__list_agents"))
        .await
        .unwrap();
    assert_eq!(r.structured_content.unwrap()["agents"][0]["name"], "up");
    assert!(seen.lock().unwrap().iter().any(|h| h == "Bearer at-1"));

    // Signing out drops the tokens and the tools.
    let (status, _) = http(&d.ui, "DELETE", &format!("/v1/connectors/{id}/oauth"), None).await;
    assert_eq!(status, 204);
    let (_, all) = http(&d.ui, "GET", "/v1/connectors", None).await;
    assert_eq!(all[0]["signed_in"], false);
    assert_eq!(all[0]["check"]["status"], "needs_sign_in", "{all}");
    let tools = as_ann.list_all_tools().await.unwrap();
    assert!(!tools.iter().any(|t| t.name.starts_with("up__")));
    as_ann.cancel().await.unwrap();

    // A server without published OAuth metadata gets no sign-in (no issuer to
    // check tokens against).
    let (bare, _) = upstream_mcp().await;
    let body = json!({"name": "bare", "url": bare});
    let (_, conn) = http(&d.ui, "POST", "/v1/connectors", Some(body)).await;
    let path = format!("/v1/connectors/{}/oauth", conn["id"].as_str().unwrap());
    let (status, body) = http(&d.ui, "POST", &path, None).await;
    assert_eq!(status, 400, "{body}");
}

async fn add_connector(d: &Daemon, body: Value) -> Value {
    let (status, conn) = http(&d.ui, "POST", "/v1/connectors", Some(body)).await;
    assert_eq!(status, 201, "{conn}");
    conn
}

#[tokio::test]
async fn connectors_with_an_own_oauth_app_need_a_sign_in_even_if_listing_is_open() {
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let url = keyless_upstream().await;
    let open = add_connector(&d, json!({"name": "open", "url": url})).await;
    assert_eq!(open["check"]["status"], "ok", "{open}");
    let app = json!({"name": "mail", "url": url, "client_id": "app.example",
        "issuer": "https://accounts.example.com"});
    let conn = add_connector(&d, app).await;
    assert_eq!(conn["check"]["status"], "needs_sign_in", "{conn}");
}

#[tokio::test]
async fn connectors_reach_servers_that_refuse_the_newest_protocol() {
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let url = legacy_upstream().await;
    let conn = add_connector(&d, json!({"name": "legacy", "url": url})).await;
    assert_eq!(conn["check"]["status"], "ok", "{conn}");
}

#[tokio::test]
async fn connectors_are_checked_when_added_changed_or_asked() {
    use rmcp::ServiceExt;
    use rmcp::transport::StreamableHttpClientTransport;
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;

    // A keyless server: connected, with the tools agents get.
    let open = keyless_upstream().await;
    let direct = ().serve(StreamableHttpClientTransport::from_uri(open.as_str()));
    let want = direct.await.unwrap().list_all_tools().await.unwrap().len();
    let conn = add_connector(&d, json!({"name": "open", "url": open})).await;
    assert_eq!(conn["check"]["status"], "ok", "{conn}");
    assert_eq!(conn["check"]["tools"], json!(want));
    assert!(conn["check"]["at"].is_string());

    // A server that wants a token and has none needs a sign-in (or a token in Vault); a wrong
    // token is a failure, never an echo of it. Fixing the token checks it again.
    let (url, _) = upstream_mcp().await;
    let conn = add_connector(&d, json!({"name": "locked", "url": url})).await;
    assert_eq!(conn["check"]["status"], "needs_sign_in", "{conn}");
    let id = conn["id"].as_str().unwrap();
    let path = format!("/v1/connectors/{id}");
    let (_, conn) = http(&d.ui, "PATCH", &path, Some(json!({"token": "tok-wrong"}))).await;
    assert_eq!(conn["check"]["status"], "failed", "{conn}");
    let error = conn["check"]["error"].as_str().unwrap();
    assert!(error.contains("rejected the token") && !error.contains("tok-wrong"));
    let (_, conn) = http(&d.ui, "PATCH", &path, Some(json!({"token": "secret"}))).await;
    assert_eq!(conn["check"]["status"], "ok", "{conn}");

    // An OAuth server that is not signed in: no discovery, no registration, no sign-in.
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = hits.clone();
    let (url, _) = upstream_with("at-1", move |base| {
        fake_auth_server(base, Arc::default()).layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                log.lock().unwrap().push(req.uri().path().to_owned());
                next.run(req)
            },
        ))
    })
    .await;
    let conn = add_connector(&d, json!({"name": "oauth", "url": url})).await;
    assert_eq!(conn["check"]["status"], "needs_sign_in", "{conn}");
    assert_eq!(*hits.lock().unwrap(), Vec::<String>::new());

    // Unreachable: failed at once, well within the bound.
    let port = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let started = std::time::Instant::now();
    let gone = format!("http://127.0.0.1:{port}/mcp");
    let conn = add_connector(&d, json!({"name": "gone", "url": gone})).await;
    assert!(started.elapsed() < Duration::from_secs(15));
    assert_eq!(conn["check"]["status"], "failed", "{conn}");
    assert_eq!(conn["check"]["error"], "cannot reach the server");

    // A server that echoes the token, at a URL holding a key: neither reaches the error.
    let (base, _) = upstream_with("unused", |_| {
        axum::Router::new().route(
            "/echo",
            axum::routing::post(|headers: HeaderMap| async move {
                let auth = headers["authorization"].to_str().unwrap().to_owned();
                (axum::http::StatusCode::BAD_REQUEST, auth)
            }),
        )
    })
    .await;
    let echo = base.replace("/mcp", "/echo?key=sekrit");
    let body = json!({"name": "echo", "url": echo, "token": "tok-zebra"});
    let conn = add_connector(&d, body).await;
    let id = conn["id"].as_str().unwrap();
    assert_eq!(conn["check"]["status"], "failed", "{conn}");
    for check in [
        conn["check"].clone(),
        http(&d.ui, "POST", &format!("/v1/connectors/{id}/check"), None)
            .await
            .1,
    ] {
        assert_eq!(check["error"], "the server answered HTTP 400", "{check}");
        let text = check.to_string();
        assert!(
            !text.contains("tok-zebra") && !text.contains("sekrit"),
            "{text}"
        );
    }

    // On demand; the last result shows in the list.
    let (status, check) = http(
        &d.ui,
        "POST",
        &format!("/v1/connectors/{}/check", AgentId::new()),
        None,
    )
    .await;
    assert_eq!(status, 404, "{check}");
    let (_, all) = http(&d.ui, "GET", "/v1/connectors", None).await;
    let statuses: Vec<&Value> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["check"]["status"])
        .collect();
    assert_eq!(statuses.len(), 5, "{all}");
    assert!(statuses.iter().all(|s| !s.is_null()), "{all}");
}

/// A stdio MCP server in a few lines of Python: one tool that says what it can see.
const FAKE_LOCAL_SERVER: &str = r#"import json, os, sys
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method = msg.get("method")
    if method == "initialize":
        res = {"protocolVersion": msg["params"]["protocolVersion"], "capabilities": {"tools": {}},
               "serverInfo": {"name": "fake", "version": "1"}}
    elif method == "tools/list":
        res = {"tools": [{"name": "whoami", "description": "What it sees",
                          "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call":
        seen = "token=%s manifest=%s cwd=%s" % (os.environ.get("FAKE_TOKEN", "-"),
            os.environ.get("CARGO_MANIFEST_DIR", "-"), os.path.basename(os.getcwd()))
        res = {"content": [{"type": "text", "text": seen}]}
    else:
        res = {}
    print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": res}), flush=True)
"#;

#[tokio::test]
async fn a_local_server_runs_here_with_only_its_own_token() {
    use rmcp::model::CallToolRequestParams;
    if std::process::Command::new("python3")
        .arg("-V")
        .output()
        .is_err()
    {
        eprintln!("skipped: no python3 for the fake local MCP server");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("server.py");
    std::fs::write(&script, FAKE_LOCAL_SERVER).unwrap();
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let ann = create(&d, json!({"name": "ann"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = json!({"name": "fake", "command": ["python3", script.to_str().unwrap()],
        "token_env": "FAKE_TOKEN", "token": "tok-local-1"});
    let (status, conn) = http(&d.ui, "POST", "/v1/connectors", Some(body)).await;
    assert_eq!(status, 201, "{conn}");
    let both = json!({"name": "both", "url": "https://x.dev/mcp", "command": ["npx"]});
    assert_eq!(
        http(&d.ui, "POST", "/v1/connectors", Some(both)).await.0,
        400
    );

    let assign = format!("/v1/agents/{ann}/connectors");
    let (status, _) = http(&d.ui, "PUT", &assign, Some(json!([conn["id"]]))).await;
    assert_eq!(status, 200);
    let as_ann = mcp_as(&d, backend.token("ann")).await;
    let tools = as_ann.list_all_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "fake__whoami"), "{tools:?}");
    let r = as_ann
        .call_tool(CallToolRequestParams::new("fake__whoami"))
        .await
        .unwrap();
    let text = serde_json::to_string(&r.content).unwrap();
    assert!(
        text.contains("token=tok-local-1"),
        "its token, as FAKE_TOKEN: {text}"
    );
    assert!(
        text.contains("manifest=-"),
        "nothing else from the daemon's environment: {text}"
    );
    assert!(text.contains("cwd=fake"), "its own folder: {text}");
}

#[tokio::test]
async fn an_agent_proposes_a_local_server_on_a_card_that_shows_its_command() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let ann = create(&d, json!({"name": "ann"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Auto mode never skips it: a local server runs code on this computer.
    let auto = Some(json!({"auto_mode": true}));
    assert_eq!(http(&d.ui, "PUT", "/v1/settings", auto).await.0, 200);
    let as_ann = mcp_as(&d, backend.token("ann")).await;
    let args = json!({"name": "notes", "command": ["npx", "-y", "@scope/notes server"],
        "token_env": "NOTES_TOKEN", "auth": "token", "reason": "Read my notes"});
    let qid = call(&as_ann, "add_connector", args).await["question_id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        http(&d.ui, "GET", "/v1/connectors", None).await.1,
        json!([])
    );
    let card = events(&d, &ann)
        .await
        .into_iter()
        .find(|e| e["id"] == qid)
        .unwrap();
    let prompt = card["payload"]["card"]["questions"][0]["prompt"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        prompt.contains("runs on this computer")
            && prompt.contains("npx -y '@scope/notes server'")
            && prompt.contains("NOTES_TOKEN"),
        "{prompt}"
    );
    let path = format!("/v1/agents/{ann}/questions/{qid}/answer");
    let approve = json!({"answers": [{"question_id": "q1", "values": ["approve"]}]});
    assert_eq!(http(&d.ui, "POST", &path, Some(approve)).await.0, 202);
    let (_, all) = http(&d.ui, "GET", "/v1/connectors", None).await;
    assert_eq!(
        all[0]["command"],
        json!(["npx", "-y", "@scope/notes server"])
    );
    assert_eq!(all[0]["token_env"], "NOTES_TOKEN");
}

#[tokio::test]
async fn a_renamed_agent_still_answers_to_its_old_name() {
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let scout = create(&d, json!({"name": "scout"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ferro = create(&d, json!({"name": "ferro"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/v1/agents/{scout}");
    let (status, _) = http(&d.ui, "PATCH", &path, Some(json!({"name": "scout-x"}))).await;
    assert_eq!(status, 200);
    let to_ferro = format!("/v1/agents/{ferro}/messages");
    let (_, sent) = http(
        &d.ui,
        "POST",
        &to_ferro,
        Some(json!({"body": "ask @scout"})),
    )
    .await;
    assert_eq!(
        sent["body"],
        format!("ask <@{scout}>"),
        "the old name finds the agent"
    );
    let found = d
        .state
        .store
        .get_agent_by_name(ferrobot_core::AgentName::parse("scout").unwrap())
        .await
        .unwrap();
    assert_eq!(
        found.name.as_str(),
        "scout-x",
        "and send_message finds it by that name"
    );
}

#[tokio::test]
async fn the_ui_hears_when_a_turn_takes_a_message() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let id = create(&d, json!({ "name": "nova" })).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut ws = connect_runner(&d.agents, &backend.token("nova")).await;
    let mut ui = d.state.ui.subscribe();
    let path = format!("/v1/agents/{id}/messages");
    let sent = http(
        &d.ui,
        "POST",
        &path,
        Some(json!({"body": "check the news"})),
    )
    .await
    .1["id"]
        .as_i64()
        .unwrap();
    next_turn(&mut ws).await;
    let in_flight = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = serde_json::to_value(ui.recv().await.unwrap()).unwrap();
            if event["type"] == "message"
                && event["message"]["id"] == sent
                && event["message"]["status"] == "in_flight"
            {
                break;
            }
        }
    })
    .await;
    assert!(
        in_flight.is_ok(),
        "no longer shown as queued once its turn runs"
    );
}

#[tokio::test]
async fn send_now_stops_only_a_turn_that_cannot_take_the_message() {
    use ferrobot_core::protocol::TurnOutcome;
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    for (name, steering) in [("quill", false), ("stella", true)] {
        let id = create(&d, json!({ "name": name })).await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut ws = connect_runner_with(&d.agents, &backend.token(name), steering).await;
        let path = format!("/v1/agents/{id}/messages");
        let post = |body: Value| {
            let (ui, path) = (d.ui.clone(), path.clone());
            async move {
                http(&ui, "POST", &path, Some(body)).await.1["id"]
                    .as_i64()
                    .unwrap()
            }
        };
        post(json!({"body": "find flights"})).await;
        let (turn_id, _, _) = next_turn(&mut ws).await;
        let now = post(json!({"body": "hotels instead", "now": true})).await;
        match next_frame(&mut ws).await {
            DaemonFrame::Interrupt { turn_id: t } if !steering => {
                assert_eq!(t, turn_id);
                let done = RunnerFrame::TurnDone {
                    turn_id,
                    outcome: TurnOutcome::Interrupted,
                    session_id: None,
                    result: None,
                    error: None,
                    steered: vec![],
                };
                ws.send(Message::text(serde_json::to_string(&done).unwrap()))
                    .await
                    .unwrap();
                let (_, message_id, body) = next_turn(&mut ws).await;
                assert_eq!((message_id, body.as_str()), (now, "hotels instead"));
            }
            DaemonFrame::Steer { message_id, .. } if steering => {
                assert_eq!(
                    message_id, now,
                    "a steering runner takes it; its turn goes on"
                );
            }
            other => panic!("{name}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn messages_reach_a_running_turn_when_the_runner_can_take_them() {
    use ferrobot_core::protocol::TurnOutcome;
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let id = create(&d, json!({"name": "stella"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut ws = connect_runner_with(&d.agents, &backend.token("stella"), true).await;
    let post = |body: &'static str| {
        let path = format!("/v1/agents/{id}/messages");
        let ui = d.ui.clone();
        async move {
            http(&ui, "POST", &path, Some(json!({"body": body})))
                .await
                .1["id"]
                .as_i64()
                .unwrap()
        }
    };
    let first = post("find flights").await;
    let DaemonFrame::Turn {
        turn_id,
        message_id,
        ..
    } = next_frame(&mut ws).await
    else {
        panic!("expected a turn");
    };
    assert_eq!(message_id, first);

    // Sent while the turn runs: handed to it right away.
    let hotels = post("also hotels").await;
    let DaemonFrame::Steer {
        turn_id: t,
        message_id: m,
        body,
        ..
    } = next_frame(&mut ws).await
    else {
        panic!("expected a steer frame");
    };
    assert_eq!(
        (t.as_str(), m, body.as_str()),
        (turn_id.as_str(), hotels, "also hotels")
    );
    let trains = post("and trains").await;
    assert!(
        matches!(next_frame(&mut ws).await, DaemonFrame::Steer { message_id, .. } if message_id == trains)
    );

    // The model saw "hotels" only: "trains" comes back as the next turn.
    let done = RunnerFrame::TurnDone {
        turn_id,
        outcome: TurnOutcome::Completed,
        session_id: None,
        result: None,
        error: None,
        steered: vec![hotels],
    };
    ws.send(Message::text(serde_json::to_string(&done).unwrap()))
        .await
        .unwrap();
    let DaemonFrame::Turn { message_id, .. } = next_frame(&mut ws).await else {
        panic!("expected the unseen message as a turn");
    };
    assert_eq!(message_id, trains);
    let (_, msgs) = http(&d.ui, "GET", &format!("/v1/agents/{id}/messages"), None).await;
    let status = |m: i64| {
        msgs.as_array()
            .unwrap()
            .iter()
            .find(|x| x["id"] == m)
            .unwrap()["status"]
            .clone()
    };
    assert_eq!(
        (status(first), status(hotels)),
        (json!("delivered"), json!("delivered"))
    );
    assert_eq!(status(trains), "in_flight");
}

#[tokio::test]
async fn the_operator_reacts_to_an_agent_message_and_the_agent_is_told() {
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let ann = create(&d, json!({"name": "ann"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ann_id: AgentId = ann.parse().unwrap();
    let said = d
        .state
        .store
        .append_event(
            ann_id,
            Some("t1".into()),
            "assistant_text".into(),
            json!({"text": "Here is the plan."}),
        )
        .await
        .unwrap();
    let react = |event: i64, emoji: &str| {
        let (ui, path) = (
            d.ui.clone(),
            format!("/v1/agents/{ann}/events/{event}/reaction"),
        );
        let body = json!({ "emoji": emoji });
        async move { http(&ui, "POST", &path, Some(body)).await.0 }
    };
    assert_eq!(react(said.id, " 👍 ").await, 204);

    let note = mail(&d, &ann).await.pop().unwrap();
    assert_eq!(
        note["body"],
        "[ferrobot] Your operator reacted 👍 to your message: \"Here is the plan.\". Act on it \
         only if it asks for something, like a fix or an answer; otherwise end your turn without \
         a reply."
    );
    assert_eq!(note["sender"]["kind"], "user");
    let shown = events(&d, &ann).await;
    let reaction = shown.iter().find(|e| e["kind"] == "user_reaction").unwrap();
    assert_eq!(
        reaction["payload"],
        json!({"event_id": said.id, "emoji": "👍", "note_id": note["id"]})
    );

    assert_eq!(react(said.id, "ok").await, 400, "one emoji only");
    let not_a_reply = d
        .state
        .store
        .append_event(ann_id, None, "file".into(), json!({}))
        .await
        .unwrap();
    assert_eq!(react(not_a_reply.id, "👍").await, 404);
    assert_eq!(react(said.id + 1000, "👍").await, 404);
}

#[tokio::test]
async fn html_blocks_from_the_chat_open_as_sandboxed_pages() {
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let html = "<style>h1{color:red}</style><h1>Hi</h1><script>document.title='x'</script>";
    let post = |body: Value| http(&d.ui, "POST", "/v1/previews", Some(body));
    let (status, link) = post(json!({ "html": html })).await;
    assert_eq!(status, 200, "{link}");
    let url = link["url"].as_str().unwrap().to_owned();
    let id = url
        .strip_prefix("/artifacts/previews/")
        .and_then(|rest| rest.split('?').next())
        .unwrap()
        .to_owned();
    let (_, again) = post(json!({ "html": html })).await;
    assert!(
        again["url"].as_str().unwrap().contains(&id),
        "the same block keeps its id"
    );

    let no_token = [("Authorization", "")];
    let (status, headers, body) = http_raw(&d.ui, "GET", &url, &no_token, b"").await;
    assert_eq!(status, 200);
    assert_eq!(String::from_utf8_lossy(&body), html);
    let csp = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-security-policy"))
        .map(|(_, v)| v.clone())
        .unwrap();
    assert!(
        csp.starts_with("sandbox allow-scripts;")
            && csp.contains("connect-src 'none'")
            && !csp.contains("allow-same-origin"),
        "{csp}"
    );

    let get = |url: String| {
        let ui = d.ui.clone();
        async move { http_raw(&ui, "GET", &url, &no_token, b"").await.0 }
    };
    let last = if url.ends_with('0') { '1' } else { '0' };
    assert_eq!(get(format!("{}{last}", &url[..url.len() - 1])).await, 404);
    assert_eq!(get(format!("/artifacts/previews/{id}")).await, 404);
    let expired = files::preview_link(&d.state, &id, 1).await.unwrap();
    assert_eq!(get(expired).await, 404, "links expire");
    let unknown = files::fresh_preview_link(&d.state, &"0".repeat(64))
        .await
        .unwrap();
    assert_eq!(get(unknown).await, 404);

    let (status, _) = post(json!({ "html": "x".repeat(files::MAX_BLOCK_BYTES + 1) })).await;
    assert_eq!(status, 400);
    let (status, _, _) =
        http_raw(&d.ui, "POST", "/v1/previews", &no_token, br#"{"html":"x"}"#).await;
    assert_eq!(status, 401, "only the operator stores blocks");
}

#[tokio::test]
async fn agents_share_files_that_open_from_storage() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let ann = create(&d, json!({"name": "ann"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ann_id: AgentId = ann.parse().unwrap();

    let plan = d.state.share_file(ann_id, "plan.md".into()).await.unwrap();
    assert_eq!(plan.name, "plan.md");
    let (status, file) = http(
        &d.ui,
        "GET",
        &format!("/v1/agents/{ann}/files/{}", plan.id),
        None,
    )
    .await;
    assert_eq!(status, 200, "{file}");
    assert_eq!(file["content"], "content of /home/node/workspace/plan.md");
    assert!(file["page_url"].is_null());
    let err = d
        .state
        .share_file(ann_id, "missing.md".into())
        .await
        .unwrap_err();
    assert!(err.contains("no file"), "{err}");
    let err = d
        .state
        .share_file(ann_id, "../etc/passwd".into())
        .await
        .unwrap_err();
    assert!(err.contains("not allowed"), "{err}");

    // An .html file opens as a sandboxed page, without the operator token.
    let page = d
        .state
        .share_file(ann_id, "~/out/report.html".into())
        .await
        .unwrap();
    let (_, file) = http(
        &d.ui,
        "GET",
        &format!("/v1/agents/{ann}/files/{}", page.id),
        None,
    )
    .await;
    let url = file["page_url"].as_str().unwrap().to_owned();
    let no_token = [("Authorization", "")];
    let (status, headers, body) = http_raw(&d.ui, "GET", &url, &no_token, b"").await;
    assert_eq!(status, 200);
    assert_eq!(
        String::from_utf8_lossy(&body),
        "content of /home/node/out/report.html"
    );
    let csp = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-security-policy"))
        .map(|(_, v)| v.clone())
        .unwrap();
    assert!(
        csp.starts_with("sandbox allow-scripts;") && csp.contains("connect-src 'none'"),
        "{csp}"
    );
    assert!(!csp.contains("https:"), "no remote origins: {csp}");
    assert!(
        !csp.contains("allow-same-origin"),
        "pages keep an opaque origin: {csp}"
    );
    let get = |url: String| {
        let ui = d.ui.clone();
        async move { http_raw(&ui, "GET", &url, &no_token, b"").await.0 }
    };
    let last = if url.ends_with('0') { '1' } else { '0' };
    let tampered = format!("{}{last}", &url[..url.len() - 1]);
    assert_eq!(get(tampered).await, 404, "the signature is the capability");
    let unsigned = format!("/artifacts/{ann}/{}", page.id);
    assert_eq!(get(unsigned).await, 404);
    let other_file = url.replacen(&format!("/{}?", page.id), &format!("/{}?", plan.id), 1);
    assert_eq!(get(other_file).await, 404, "a link is bound to its file");
    let expired = files::signed_link(&d.state, ann_id, page.id, 1)
        .await
        .unwrap();
    assert_eq!(get(expired).await, 404, "links expire");

    // A file event pointing outside the agent's own files is never served.
    let bob: AgentId = create(&d, json!({"name": "bob"})).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let bob_file = d.state.share_file(bob, "secret.md".into()).await.unwrap();
    let bob_key = d
        .state
        .store
        .get_event(bob, bob_file.id)
        .await
        .unwrap()
        .payload["key"]
        .as_str()
        .unwrap()
        .to_owned();
    for key in [bob_key, format!("artifacts/{ann}/../{bob}/x")] {
        let forged = d
            .state
            .store
            .append_event(
                ann_id,
                None,
                "file".into(),
                json!({"name": "x.md", "size": 1, "path": "x", "key": key}),
            )
            .await
            .unwrap();
        let (status, _) = http(
            &d.ui,
            "GET",
            &format!("/v1/agents/{ann}/files/{}", forged.id),
            None,
        )
        .await;
        assert_eq!(status, 404, "{key}");
        let link = files::fresh_link(&d.state, ann_id, forged.id)
            .await
            .unwrap();
        assert_eq!(get(link).await, 404, "{key}");
    }

    // Any shared file downloads from its url; .html too with ?download.
    let (_, md) = http(
        &d.ui,
        "GET",
        &format!("/v1/agents/{ann}/files/{}", plan.id),
        None,
    )
    .await;
    let md_url = md["url"].as_str().unwrap().to_owned();
    let (status, headers, body) = http_raw(&d.ui, "GET", &md_url, &no_token, b"").await;
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    assert_eq!(status, 200);
    assert_eq!(body, b"content of /home/node/workspace/plan.md");
    assert_eq!(
        header("content-disposition"),
        "attachment; filename=\"plan.md\""
    );
    assert!(header("content-security-policy").starts_with("sandbox"));
    let (_, headers, _) =
        http_raw(&d.ui, "GET", &format!("{url}&download=1"), &no_token, b"").await;
    assert!(
        headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-disposition")
                && v.starts_with("attachment"))
    );

    // A .pdf name alone is not enough to open inline: the content must be a PDF.
    let fake = d.state.share_file(ann_id, "fake.pdf".into()).await.unwrap();
    let (_, f) = http(
        &d.ui,
        "GET",
        &format!("/v1/agents/{ann}/files/{}", fake.id),
        None,
    )
    .await;
    let (_, headers, _) = http_raw(&d.ui, "GET", f["url"].as_str().unwrap(), &no_token, b"").await;
    assert!(headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v == "application/octet-stream"));

    // Deleting the agent deletes its files.
    for id in [ann_id, bob] {
        let (status, _) = http(&d.ui, "DELETE", &format!("/v1/agents/{id}"), None).await;
        assert_eq!(status, 204);
    }
    let left = d.state.settings.storage.list(None).count().await;
    assert_eq!(left, 0);
}

/// What the fake provider with its own OAuth app saw and should do.
#[derive(Default)]
struct AppServer {
    challenge: Option<String>,
    reject_refresh: bool,
    /// `grant_type` of each token request, with whether the client
    /// authenticated with the app's secret.
    grants: Vec<(String, bool)>,
    revoked: Vec<(String, bool)>,
}

/// An authorization server like Google's: a pre-registered client (`client-app` /
/// `app-secret`), short-lived access tokens, refresh and RFC 7009 revocation.
fn fake_app_server(base: &str, log: Arc<Mutex<AppServer>>) -> axum::Router {
    use axum::routing::{get, post};
    use base64::Engine as _;
    use sha2::Digest as _;
    let json = |v: Value| {
        move || {
            let v = v.clone();
            async move { axum::Json(v) }
        }
    };
    let prm = json!({"resource": format!("{base}/mcp"), "authorization_servers": [base]});
    let meta = json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "revocation_endpoint": format!("{base}/revoke"),
        "registration_endpoint": format!("{base}/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
    });
    let authed = |headers: &HeaderMap, form: &HashMap<String, String>| {
        let basic = base64::engine::general_purpose::STANDARD.encode("client-app:app-secret");
        let header = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == format!("Basic {basic}"));
        header || form.get("client_secret").map(String::as_str) == Some("app-secret")
    };
    let token_log = log.clone();
    let token = move |headers: HeaderMap, body: String| {
        let log = token_log.clone();
        async move {
            let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
                .into_owned()
                .collect();
            let grant = form.get("grant_type").cloned().unwrap_or_default();
            let ok_client = authed(&headers, &form);
            let mut log = log.lock().unwrap();
            log.grants.push((grant.clone(), ok_client));
            let bad = |e: &str| {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(json!({"error": e})),
                )
            };
            if !ok_client {
                return bad("invalid_client");
            }
            let tokens = json!({"access_token": "at-1", "token_type": "Bearer",
                                "expires_in": 1, "refresh_token": "rt-1"});
            match grant.as_str() {
                "authorization_code" => {
                    let verifier = form.get("code_verifier").cloned().unwrap_or_default();
                    let derived = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(sha2::Sha256::digest(verifier.as_bytes()));
                    if log.challenge.as_deref() != Some(derived.as_str()) {
                        return bad("invalid_grant");
                    }
                    (axum::http::StatusCode::OK, axum::Json(tokens))
                }
                "refresh_token"
                    if !log.reject_refresh
                        && form.get("refresh_token").map(String::as_str) == Some("rt-1") =>
                {
                    (axum::http::StatusCode::OK, axum::Json(tokens))
                }
                _ => bad("invalid_grant"),
            }
        }
    };
    let revoke_log = log;
    let revoke = move |headers: HeaderMap, body: String| {
        let log = revoke_log.clone();
        async move {
            let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
                .into_owned()
                .collect();
            let ok_client = authed(&headers, &form);
            let token = form.get("token").cloned().unwrap_or_default();
            log.lock().unwrap().revoked.push((token, ok_client));
            axum::http::StatusCode::OK
        }
    };
    axum::Router::new()
        .route("/.well-known/oauth-protected-resource/mcp", get(json(prm)))
        .route("/.well-known/oauth-authorization-server", get(json(meta)))
        .route("/token", post(token))
        .route("/revoke", post(revoke))
        .route(
            "/register",
            post(|body: axum::Json<Value>| async move {
                let client = json!({"client_id": "client-app", "client_secret": "app-secret",
                                    "redirect_uris": body["redirect_uris"],
                                    "token_endpoint_auth_method": "client_secret_basic"});
                (axum::http::StatusCode::CREATED, axum::Json(client))
            }),
        )
}

#[tokio::test]
async fn connectors_keep_the_secret_a_server_registered_them_with() {
    use rmcp::model::CallToolRequestParams;
    let log = Arc::new(Mutex::new(AppServer::default()));
    let server_log = log.clone();
    let (url, _) = upstream_with("at-1", move |base| fake_app_server(base, server_log)).await;
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let ann = create(&d, json!({"name": "ann"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = json!({"name": "reg", "url": url});
    let (_, conn) = http(&d.ui, "POST", "/v1/connectors", Some(body)).await;
    let id = conn["id"].as_str().unwrap().to_owned();
    let cid: uuid::Uuid = id.parse().unwrap();
    http(
        &d.ui,
        "PUT",
        &format!("/v1/agents/{ann}/connectors"),
        Some(json!([id])),
    )
    .await;

    // Dynamic registration hands out a confidential client.
    let (status, started) = http(&d.ui, "POST", &format!("/v1/connectors/{id}/oauth"), None).await;
    assert_eq!(status, 200, "{started}");
    let auth = url::Url::parse(started["authorization_url"].as_str().unwrap()).unwrap();
    let q: HashMap<String, String> = auth.query_pairs().into_owned().collect();
    assert_eq!(q["client_id"], "client-app");
    let registration = ferrobot_daemon::oauth::registration_key(cid);
    let secrets = &d.state.settings.secrets;
    assert!(
        secrets.get(&registration).await.unwrap().is_none(),
        "kept only once the sign-in succeeds"
    );
    log.lock().unwrap().challenge = Some(q["code_challenge"].clone());
    let back = format!("/v1/oauth/callback?code=code-1&state={}", q["state"]);
    let (status, _, _) = http_raw(&d.ui, "GET", &back, &[("Authorization", "")], b"").await;
    assert_eq!(status, 200);
    let (_, all) = http(&d.ui, "GET", "/v1/connectors", None).await;
    assert_eq!(all[0]["sign_in"], "signed_in");
    assert_eq!(all[0]["client_id"], Value::Null, "not an own app");
    assert!(!all.to_string().contains("app-secret"));

    // A fresh session refreshes the expired token with the stored secret.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    d.state.connectors.evict_connector(cid).await;
    let as_ann = mcp_as(&d, backend.token("ann")).await;
    let r = as_ann
        .call_tool(CallToolRequestParams::new("reg__list_agents"))
        .await
        .unwrap();
    assert_eq!(r.structured_content.unwrap()["agents"][0]["name"], "up");
    as_ann.cancel().await.unwrap();
    let grants = log.lock().unwrap().grants.clone();
    assert!(
        grants.iter().any(|(g, _)| g == "refresh_token"),
        "{grants:?}"
    );
    assert!(grants.iter().all(|(_, ok)| *ok), "{grants:?}");

    // Signing out forgets the registration with the tokens.
    let (status, _) = http(&d.ui, "DELETE", &format!("/v1/connectors/{id}/oauth"), None).await;
    assert_eq!(status, 204);
    assert!(secrets.get(&registration).await.unwrap().is_none());
}

#[tokio::test]
async fn connectors_use_an_own_oauth_app_refresh_and_revoke() {
    use rmcp::model::CallToolRequestParams;
    let log = Arc::new(Mutex::new(AppServer::default()));
    let server_log = log.clone();
    let (url, _) = upstream_with("at-1", move |base| fake_app_server(base, server_log)).await;
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let ann = create(&d, json!({"name": "ann"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let issuer = url.trim_end_matches("/mcp").to_owned();
    let body = json!({"name": "app", "url": url, "client_id": "client-app", "issuer": issuer,
                      "client_secret": "app-secret", "scopes": ["mail.read mail.read", "mail.compose"]});
    let (status, conn) = http(&d.ui, "POST", "/v1/connectors", Some(body)).await;
    assert_eq!(status, 201, "{conn}");
    assert_eq!(conn["client_id"], "client-app");
    assert_eq!(conn["has_client_secret"], true);
    assert_eq!(conn["scopes"], json!(["mail.read", "mail.compose"]));
    assert_eq!(
        conn["redirect_uri"],
        format!("http://{}/v1/oauth/callback", d.ui)
    );
    assert!(!conn.to_string().contains("app-secret"), "{conn}");
    let id = conn["id"].as_str().unwrap().to_owned();
    http(
        &d.ui,
        "PUT",
        &format!("/v1/agents/{ann}/connectors"),
        Some(json!([id])),
    )
    .await;

    // Sign in with the app's client id; no dynamic registration happens.
    let (status, started) = http(&d.ui, "POST", &format!("/v1/connectors/{id}/oauth"), None).await;
    assert_eq!(status, 200, "{started}");
    let auth = url::Url::parse(started["authorization_url"].as_str().unwrap()).unwrap();
    let q: HashMap<String, String> = auth.query_pairs().into_owned().collect();
    assert_eq!(q["client_id"], "client-app");
    assert_eq!(q["scope"], "mail.read mail.compose");
    log.lock().unwrap().challenge = Some(q["code_challenge"].clone());
    let back = format!("/v1/oauth/callback?code=code-1&state={}", q["state"]);
    let (status, _, _) = http_raw(&d.ui, "GET", &back, &[("Authorization", "")], b"").await;
    assert_eq!(status, 200);
    let (_, all) = http(&d.ui, "GET", "/v1/connectors", None).await;
    assert_eq!(all[0]["sign_in"], "signed_in");

    // The access token lives a second: the next call refreshes, with the secret.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let as_ann = mcp_as(&d, backend.token("ann")).await;
    let r = as_ann
        .call_tool(CallToolRequestParams::new("app__list_agents"))
        .await
        .unwrap();
    assert_eq!(r.structured_content.unwrap()["agents"][0]["name"], "up");
    let grants = log.lock().unwrap().grants.clone();
    assert!(grants.iter().all(|(_, ok)| *ok), "{grants:?}");
    assert!(
        grants.iter().any(|(g, _)| g == "refresh_token"),
        "{grants:?}"
    );
    as_ann.cancel().await.unwrap();

    // The server rejects the refresh token: the connector says so.
    log.lock().unwrap().reject_refresh = true;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    d.state
        .connectors
        .evict_connector(id.parse().unwrap())
        .await;
    let as_ann = mcp_as(&d, backend.token("ann")).await;
    let _ = as_ann
        .call_tool(CallToolRequestParams::new("app__list_agents"))
        .await;
    as_ann.cancel().await.unwrap();
    let (_, all) = http(&d.ui, "GET", "/v1/connectors", None).await;
    assert_eq!(all[0]["sign_in"], "expired", "{all}");

    // Sign in again, then sign out. Revocation needs https (this fake server is
    // http), so the tokens are only forgotten here; unit tests cover the rule.
    let (_, started) = http(&d.ui, "POST", &format!("/v1/connectors/{id}/oauth"), None).await;
    let auth = url::Url::parse(started["authorization_url"].as_str().unwrap()).unwrap();
    let q: HashMap<String, String> = auth.query_pairs().into_owned().collect();
    log.lock().unwrap().challenge = Some(q["code_challenge"].clone());
    let back = format!("/v1/oauth/callback?code=code-1&state={}", q["state"]);
    let (status, _, _) = http_raw(&d.ui, "GET", &back, &[("Authorization", "")], b"").await;
    assert_eq!(status, 200);
    let (status, _) = http(&d.ui, "DELETE", &format!("/v1/connectors/{id}/oauth"), None).await;
    assert_eq!(status, 204);
    assert!(
        log.lock().unwrap().revoked.is_empty(),
        "no token over plain http"
    );
    let (_, all) = http(&d.ui, "GET", "/v1/connectors", None).await;
    assert_eq!(all[0]["sign_in"], "signed_out");

    // An own app needs its issuer; a server naming another one gets nothing.
    for bad in [
        json!({"name": "x", "url": url, "client_secret": "s"}),
        json!({"name": "x", "url": url, "client_id": "c"}),
        json!({"name": "x", "url": url, "client_id": "c", "issuer": "http://evil.example"}),
        json!({"name": "x", "url": url, "scopes": (0..40).map(|i| format!("s{i}")).collect::<Vec<_>>()}),
    ] {
        let (status, _) = http(&d.ui, "POST", "/v1/connectors", Some(bad.clone())).await;
        assert_eq!(status, 400, "{bad}");
    }
    let body = json!({"name": "mixup", "url": url, "client_id": "client-app",
                      "client_secret": "app-secret", "issuer": "https://accounts.google.com"});
    let (_, mixup) = http(&d.ui, "POST", "/v1/connectors", Some(body)).await;
    let (status, err) = http(
        &d.ui,
        "POST",
        &format!("/v1/connectors/{}/oauth", mixup["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, 400, "{err}");
    assert!(err["message"].as_str().unwrap().contains("issuer"), "{err}");

    // A new client id without a new secret drops the old app's secret.
    let (_, conn) = http(
        &d.ui,
        "PATCH",
        &format!("/v1/connectors/{id}"),
        Some(json!({"client_id": "other-app"})),
    )
    .await;
    assert_eq!(conn["has_client_secret"], false, "{conn}");
    let (status, conn) = http(
        &d.ui,
        "PATCH",
        &format!("/v1/connectors/{id}"),
        Some(json!({"client_id": null})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        (conn["client_id"].clone(), conn["has_client_secret"].clone()),
        (Value::Null, json!(false))
    );
}

type McpClient = rmcp::service::RunningService<rmcp::RoleClient, ()>;

/// Calls one of ferrobot's own tools; returns its structured result.
async fn call(client: &McpClient, tool: &'static str, args: Value) -> Value {
    let args = args.as_object().cloned().unwrap();
    let request = rmcp::model::CallToolRequestParams::new(tool).with_arguments(args);
    client
        .call_tool(request)
        .await
        .unwrap()
        .structured_content
        .unwrap()
}

/// Ends a fake runner's turn successfully with `result` as its final answer.
async fn finish(ws: &mut Ws, turn_id: String, result: Option<&str>) {
    let done = RunnerFrame::TurnDone {
        turn_id,
        outcome: ferrobot_core::protocol::TurnOutcome::Completed,
        session_id: None,
        result: result.map(Into::into),
        error: None,
        steered: vec![],
    };
    ws.send(Message::text(serde_json::to_string(&done).unwrap()))
        .await
        .unwrap();
}

async fn next_turn(ws: &mut Ws) -> (String, i64, String) {
    match next_frame(ws).await {
        DaemonFrame::Turn {
            turn_id,
            message_id,
            body,
            ..
        } => (turn_id, message_id, body),
        other => panic!("expected a turn, got {other:?}"),
    }
}

/// Messages to or from `agent`.
async fn mail(d: &Daemon, agent: &str) -> Vec<Value> {
    let (_, msgs) = http(&d.ui, "GET", &format!("/v1/agents/{agent}/messages"), None).await;
    msgs.as_array().unwrap().clone()
}

/// Waits until message `id` to `agent` is delivered.
async fn delivered(d: &Daemon, agent: &str, id: i64) {
    eventually("delivered", Duration::from_secs(5), || async {
        mail(d, agent)
            .await
            .iter()
            .any(|m| m["id"] == id && m["status"] == "delivered")
            .then_some(())
    })
    .await;
}

/// Bodies of the messages to `to` threaded to message `id`.
async fn replies(d: &Daemon, to: &str, id: i64) -> Vec<Value> {
    mail(d, to)
        .await
        .into_iter()
        .filter(|m| m["recipient"] == to && m["reply_to"] == id)
        .map(|m| m["body"].clone())
        .collect()
}

#[tokio::test]
async fn agent_mail_wakes_sleeping_agents() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    create(&d, json!({"name": "ada"})).await;
    let as_ada = mcp_as(&d, backend.token("ada")).await;
    let tools = as_ada.list_all_tools().await.unwrap();
    for (tool, says) in [
        ("send_message", "wakes a sleeping recipient"),
        (
            "assign_task",
            "comes back to you as a message automatically",
        ),
        ("list_tasks", "with their status"),
        ("create_agent", "`scout` for research"),
    ] {
        let description = tools
            .iter()
            .find(|t| t.name == tool)
            .unwrap()
            .description
            .clone();
        assert!(description.unwrap_or_default().contains(says), "{tool}");
    }
    for (name, level, asleep) in [
        ("wren", SleepLevel::Warm, "paused"),
        ("cole", SleepLevel::Cold, "stopped"),
        ("arch", SleepLevel::Archived, "archived"),
    ] {
        let id = create(&d, json!({"name": name})).await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        ferrobot_daemon::lifecycle::sleep(&d.state, id.parse().unwrap(), "test", level)
            .await
            .unwrap();
        let path = format!("/v1/agents/{id}");
        assert_eq!(http(&d.ui, "GET", &path, None).await.1["status"], asleep);
        call(
            &as_ada,
            "send_message",
            json!({"to": name, "body": "wake up"}),
        )
        .await;
        eventually(name, Duration::from_secs(5), || async {
            (http(&d.ui, "GET", &path, None).await.1["status"] == "running").then_some(())
        })
        .await;
    }
}

#[tokio::test]
async fn tasks_report_back_once_and_never_loop() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let id_of = |v: Value| v["id"].as_str().unwrap().to_owned();
    let ada = id_of(create(&d, json!({"name": "ada"})).await);
    let bob = id_of(create(&d, json!({"name": "bob"})).await);
    let as_ada = mcp_as(&d, backend.token("ada")).await;
    let as_bob = mcp_as(&d, backend.token("bob")).await;
    let mut bob_ws = connect_runner(&d.agents, &backend.token("bob")).await;
    let assign = || {
        call(
            &as_ada,
            "assign_task",
            json!({"to": "bob", "task": "find flights"}),
        )
    };

    // (bob's own reply to the task, bob's final answer, what reaches ada)
    for (reply, answer, expected) in [
        (None, Some("3 flights"), "Task #{id} answer:\n\n3 flights"),
        (
            Some("here they are"),
            Some("Sent ada the flights."),
            "here they are",
        ),
    ] {
        let task = assign().await["message_id"].as_i64().unwrap();
        let (turn, message_id, _) = next_turn(&mut bob_ws).await;
        assert_eq!(message_id, task);
        if let Some(reply) = reply {
            let args = json!({"to": ada, "body": reply, "reply_to": task});
            call(&as_bob, "send_message", args).await;
        }
        finish(&mut bob_ws, turn, answer).await;
        delivered(&d, &bob, task).await;
        let expected = expected.replace("{id}", &task.to_string());
        assert_eq!(replies(&d, &ada, task).await, [json!(expected)]);
    }

    // A turn that ends without an answer gets exactly one reminder.
    let task = assign().await["message_id"].as_i64().unwrap();
    let (turn, _, _) = next_turn(&mut bob_ws).await;
    finish(&mut bob_ws, turn, None).await;
    delivered(&d, &bob, task).await;
    assert_eq!(
        replies(&d, &ada, task).await.len(),
        0,
        "no answer to pass on"
    );
    ferrobot_daemon::supervisor::check_all(&d.state).await;
    let (turn, nudge, body) = next_turn(&mut bob_ws).await;
    let reminder = format!("Automatic reminder: task #{task}");
    assert!(body.starts_with(&reminder), "{body}");
    finish(&mut bob_ws, turn, None).await;
    delivered(&d, &bob, nudge).await;
    ferrobot_daemon::supervisor::check_all(&d.state).await;
    assert_eq!(replies(&d, &bob, task).await.len(), 1, "one reminder only");
    let list = call(&as_ada, "list_tasks", json!({})).await;
    assert_eq!(list["given"][0]["status"], "open", "{list}");
    // An answer threaded to the reminder counts for the task.
    let args = json!({"to": ada, "body": "found none", "reply_to": nudge});
    call(&as_bob, "send_message", args).await;
    let list = call(&as_ada, "list_tasks", json!({})).await;
    let given = list["given"].as_array().unwrap();
    assert_eq!(given.len(), 3, "{list}");
    assert!(given.iter().all(|t| t["status"] == "done"), "{list}");
    let list = call(&as_bob, "list_tasks", json!({})).await;
    assert_eq!(list["received"][0]["agent"], "ada", "{list}");

    // Answers are no tasks: ada's turns on them send bob nothing back.
    let mut ada_ws = connect_runner(&d.agents, &backend.token("ada")).await;
    for _ in 0..3 {
        let (turn, _, _) = next_turn(&mut ada_ws).await;
        finish(&mut ada_ws, turn, Some("Thanks, bob.")).await;
    }
    eventually("ada done", Duration::from_secs(5), || async {
        mail(&d, &ada)
            .await
            .iter()
            .all(|m| m["recipient"] != ada.as_str() || m["status"] == "delivered")
            .then_some(())
    })
    .await;
    ferrobot_daemon::supervisor::check_all(&d.state).await;
    let quiet = tokio::time::timeout(Duration::from_millis(300), bob_ws.next()).await;
    assert!(quiet.is_err(), "bob got another turn: {quiet:?}");
    let to_bob = mail(&d, &bob)
        .await
        .iter()
        .filter(|m| m["recipient"] == bob.as_str())
        .count();
    assert_eq!(to_bob, 4, "three tasks and one reminder");
}

#[tokio::test]
async fn new_agents_greet_the_operator_first() {
    use ferrobot_daemon::runner_ws::GREETING;
    let backend = Arc::new(StatefulBackend::default());
    let d = start_with(backend.clone(), None, "127.0.0.1:0", |s| {
        s.greet_new_agents = true;
    })
    .await;
    let post = |id: String, body: &'static str| {
        let ui = d.ui.clone();
        async move {
            let path = format!("/v1/agents/{id}/messages");
            http(&ui, "POST", &path, Some(json!({"body": body})))
                .await
                .1["id"]
                .as_i64()
                .unwrap()
        }
    };
    // Created by the operator, with and without a message queued before its runner connects.
    for (name, queued) in [("gia", None), ("hal", Some("plan the trip"))] {
        let id = create(&d, json!({"name": name})).await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let queued_id = match queued {
            Some(body) => post(id.clone(), body).await,
            None => 0,
        };
        let mut ws = connect_runner(&d.agents, &backend.token(name)).await;
        let (turn, message_id, body) = next_turn(&mut ws).await;
        let expected = queued.map_or(GREETING.to_owned(), |q| format!("{GREETING}\n\n{q}"));
        assert_eq!((message_id, body), (queued_id, expected), "{name}");
        finish(&mut ws, turn, Some("Hi, I plan trips.")).await;
        post(id, "next").await;
        assert_eq!(next_turn(&mut ws).await.2, "next", "{name}: greets once");
    }

    // Created by an agent: no turn of its own; its first message opens with the greeting.
    let gia: AgentId = http(&d.ui, "GET", "/v1/agents", None).await.1[0]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let spec = ferrobot_daemon::agents::AgentSpec {
        name: "ivy".into(),
        description: String::new(),
        instructions: String::new(),
        character: None,
        provider_id: None,
        model: None,
        tools: None,
        connectors: None,
        tags: Vec::new(),
    };
    let ivy = ferrobot_daemon::agents::create(&d.state, spec, Some(gia))
        .await
        .unwrap();
    let mut ws = connect_runner(&d.agents, &backend.token("ivy")).await;
    let quiet = tokio::time::timeout(Duration::from_millis(300), ws.next()).await;
    assert!(quiet.is_err(), "no greeting turn: {quiet:?}");
    let task = d
        .state
        .store
        .enqueue_task(gia, ivy.id, "book flights".into())
        .await
        .unwrap();
    d.state.message_enqueued(&task);
    let (_, _, body) = next_turn(&mut ws).await;
    assert_eq!(body, format!("{GREETING}\n\nbook flights"));
}

/// A fake `ElevenLabs` + `OpenRouter` that records what reached it.
async fn voice_upstream() -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let log = seen.clone();
    let handler = move |request: axum::extract::Request| {
        let log = log.clone();
        async move {
            let (parts, body) = request.into_parts();
            let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
            let body = String::from_utf8_lossy(&body).into_owned();
            let header = |name: &str| parts.headers.get(name).and_then(|v| v.to_str().ok());
            let path = parts.uri.path().to_owned();
            log.lock().unwrap().push(json!({
                "path": path,
                "query": parts.uri.query(),
                "xi_api_key": header("xi-api-key"),
                "authorization": header("authorization"),
                "content_type": header("content-type"),
                "body": body,
            }));
            if body.contains("fail") {
                (
                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    "voice_not_found",
                )
                    .into_response()
            } else if path.ends_with("speech-to-text") || path.ends_with("transcriptions") {
                axum::Json(json!({"text": " hello there ", "language_code": "en"})).into_response()
            } else if path == "/v2/voices" {
                let more = !parts
                    .uri
                    .query()
                    .unwrap_or_default()
                    .contains("next_page_token");
                let name = if more { "Zed" } else { "Ada" };
                axum::Json(
                    json!({"voices": [{"voice_id": format!("id-{name}"), "name": name,
                    "verified_languages": [{"language": "de"}]}],
                    "has_more": more, "next_page_token": "page2"}),
                )
                .into_response()
            } else if path == "/v1/models" {
                axum::Json(json!([{"model_id": "eleven_flash_v2_5", "name": "Flash",
                    "can_do_text_to_speech": true, "languages": [{"language_id": "de", "name": "German"}]}]))
                .into_response()
            } else if path == "/v1/single-use-token/realtime_scribe" {
                axum::Json(json!({"token": "single-use-1"})).into_response()
            } else {
                ([("content-type", "audio/mpeg")], &b"ID3-fake-mp3"[..]).into_response()
            }
        }
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().fallback(handler);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

async fn speak(d: &Daemon, text: &str) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let body = json!({ "text": text }).to_string();
    http_raw(
        &d.ui,
        "POST",
        "/v1/voice/speech",
        &[("content-type", "application/json")],
        body.as_bytes(),
    )
    .await
}

async fn put_voice(d: &Daemon, body: Value) -> (u16, Value) {
    http(&d.ui, "PUT", "/v1/voice", Some(body)).await
}

#[tokio::test]
async fn voice_speaks_and_listens_with_keys_kept_in_the_daemon() {
    let (upstream, seen) = voice_upstream().await;
    let d = start_with(
        Arc::new(StatefulBackend::default()),
        None,
        "127.0.0.1:0",
        |s| s.voice_upstream = Some(upstream),
    )
    .await;
    let last = || seen.lock().unwrap().last().cloned().unwrap();

    let (status, voice) = http(&d.ui, "GET", "/v1/voice", None).await;
    assert_eq!(status, 200);
    assert_eq!(voice["provider"], Value::Null);
    assert_eq!(voice["providers"][0]["has_key"], false);
    assert_eq!(speak(&d, "Hallo").await.0, 409, "not set up yet");

    let (status, voice) = put_voice(
        &d,
        json!({"provider": "elevenlabs", "api_key": "xi-secret"}),
    )
    .await;
    assert_eq!(status, 200, "{voice}");
    assert_eq!(voice["providers"][0]["kind"], "elevenlabs");
    assert_eq!(voice["providers"][0]["has_key"], true);
    assert!(!voice.to_string().contains("xi-secret"));
    assert_eq!(speak(&d, "Hallo").await.0, 409, "ElevenLabs needs a voice");
    let (status, _) = put_voice(
        &d,
        json!({"provider": "elevenlabs", "voice": "../v1/history"}),
    )
    .await;
    assert_eq!(status, 400, "voices stay out of other paths");
    let (status, voice) =
        put_voice(&d, json!({"provider": "elevenlabs", "voice": "voice123"})).await;
    assert_eq!(status, 200);
    assert_eq!(
        voice["providers"][0]["has_key"], true,
        "omitted key is kept"
    );

    let (status, headers, audio) = speak(&d, "Hallo").await;
    assert_eq!(status, 200);
    assert!(headers.contains(&("content-type".into(), "audio/mpeg".into())));
    assert_eq!(audio, b"ID3-fake-mp3");
    let call = last();
    assert_eq!(call["path"], "/v1/text-to-speech/voice123/stream");
    assert_eq!(call["query"], "output_format=mp3_44100_128");
    assert_eq!(call["xi_api_key"], "xi-secret");
    let sent: Value = serde_json::from_str(call["body"].as_str().unwrap()).unwrap();
    assert_eq!(
        sent,
        json!({"text": "Hallo", "model_id": "eleven_flash_v2_5"})
    );

    // The same speech as a link an audio element can stream, with the token in the query.
    let link = format!(
        "/v1/voice/speech?text=Gr%C3%BC%C3%9F%20dich&access_token={}",
        common::TEST_TOKEN
    );
    let no_header = [("authorization", "")];
    let (status, headers, audio) = http_raw(&d.ui, "GET", &link, &no_header, b"").await;
    assert_eq!(status, 200);
    assert!(headers.contains(&("content-type".into(), "audio/mpeg".into())));
    assert_eq!(audio, b"ID3-fake-mp3");
    let sent: Value = serde_json::from_str(last()["body"].as_str().unwrap()).unwrap();
    assert_eq!(sent["text"], "Grüß dich");
    let (status, _, _) = http_raw(&d.ui, "GET", "/v1/voice/speech?text=hi", &no_header, b"").await;
    assert_eq!(status, 401, "the link still needs the token");

    let (status, _, out) = http_raw(
        &d.ui,
        "POST",
        "/v1/voice/transcription",
        &[("content-type", "audio/webm;codecs=opus")],
        b"webm-bytes",
    )
    .await;
    assert_eq!(status, 200);
    let out: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(out, json!({"text": "hello there"}));
    let call = last();
    assert_eq!(call["path"], "/v1/speech-to-text");
    assert!(
        call["content_type"]
            .as_str()
            .unwrap()
            .starts_with("multipart/form-data; boundary=")
    );
    let form = call["body"].as_str().unwrap();
    assert!(
        form.contains("scribe_v3") && form.contains("webm-bytes"),
        "{form}"
    );

    // Limits: text length, audio type and size; provider errors pass on.
    assert_eq!(speak(&d, &"a".repeat(4097)).await.0, 400);
    let transcribe = |mime: &'static str, audio: Vec<u8>| {
        let ui = d.ui.clone();
        async move {
            http_raw(
                &ui,
                "POST",
                "/v1/voice/transcription",
                &[("content-type", mime)],
                &audio,
            )
            .await
            .0
        }
    };
    assert_eq!(transcribe("video/mp4", b"x".to_vec()).await, 400);
    assert_eq!(
        transcribe("audio/webm", vec![0; 10 * 1024 * 1024 + 1]).await,
        413
    );
    let (status, _, out) = speak(&d, "fail").await;
    assert_eq!(status, 502);
    assert!(
        String::from_utf8_lossy(&out).contains("voice_not_found"),
        "the provider's reason reaches the operator"
    );

    // OpenRouter: bearer key, its default voice, mp3.
    let (status, voice) = put_voice(
        &d,
        json!({"provider": "openrouter", "api_key": "sk-or-secret"}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(voice["providers"][1]["has_key"], true);
    assert_eq!(speak(&d, "Hi").await.0, 200);
    let call = last();
    assert_eq!(call["path"], "/audio/speech");
    assert_eq!(call["authorization"], "Bearer sk-or-secret");
    let sent: Value = serde_json::from_str(call["body"].as_str().unwrap()).unwrap();
    assert_eq!(sent["voice"], "en_paul_neutral");
    assert_eq!(sent["response_format"], "mp3");

    // A stuck UI cannot keep spending: about a minute's budget, then 429.
    assert_eq!(speak(&d, &"b".repeat(4096)).await.0, 200);
    let (status, _, out) = speak(&d, &"c".repeat(4096)).await;
    assert_eq!(status, 429, "{}", String::from_utf8_lossy(&out));

    let (status, voice) = put_voice(&d, json!({"provider": "openrouter", "api_key": null})).await;
    assert_eq!(status, 200);
    assert_eq!(
        voice["providers"][1]["has_key"], false,
        "null removes the key"
    );
    assert_eq!(voice["providers"][0]["has_key"], true);

    // Agents in a call can speak with their own voice.
    let agent = "00000000-0000-4000-8000-000000000001";
    let (status, _) = put_voice(
        &d,
        json!({"provider": "elevenlabs", "voice": "voice123", "agent_voices": {agent: "bobvoice"}}),
    )
    .await;
    assert_eq!(status, 200);
    let body = json!({ "text": "Hi", "agent_id": agent }).to_string();
    let (status, _, _) = http_raw(
        &d.ui,
        "POST",
        "/v1/voice/speech",
        &[("content-type", "application/json")],
        body.as_bytes(),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(last()["path"], "/v1/text-to-speech/bobvoice/stream");

    // In a call, an agent without a voice of its own speaks with one nobody else there uses.
    let host = create(&d, json!({"name": "hal"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let guest = create(&d, json!({"name": "gus"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let both = Some(json!({"agents": [host, guest]}));
    assert_eq!(http(&d.ui, "PUT", "/v1/call", both).await.0, 200);
    for (who, voice) in [(&host, "voice123"), (&guest, "id-Ada")] {
        let body = json!({ "text": "Hi", "agent_id": who }).to_string();
        let headers = [("content-type", "application/json")];
        let (status, _, _) =
            http_raw(&d.ui, "POST", "/v1/voice/speech", &headers, body.as_bytes()).await;
        assert_eq!(status, 200);
        assert_eq!(last()["path"], format!("/v1/text-to-speech/{voice}/stream"));
    }
    let hang_up = Some(json!({"agents": []}));
    assert_eq!(http(&d.ui, "PUT", "/v1/call", hang_up).await.0, 200);

    // The account's voices (all pages) and speech models, fetched with the key.
    let (status, catalog) = http(&d.ui, "GET", "/v1/voice/catalog", None).await;
    assert_eq!(status, 200, "{catalog}");
    let names: Vec<&str> = catalog["voices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Ada", "Zed"], "both pages, sorted");
    assert_eq!(catalog["tts_models"][0]["languages"][0]["code"], "de");
    assert_eq!(last()["xi_api_key"], "xi-secret");

    // Live transcription gets a single-use token; the key stays here.
    let live = json!({"mode": "dictation", "sample_rate": 16000});
    let (status, session) = http(&d.ui, "POST", "/v1/voice/live", Some(live)).await;
    assert_eq!(status, 200, "{session}");
    let url = session["url"].as_str().unwrap();
    assert!(
        url.starts_with("ws://") && url.contains("token=single-use-1"),
        "{url}"
    );
    assert!(!session.to_string().contains("xi-secret"));
    let bad = json!({"mode": "call", "sample_rate": 11025});
    assert_eq!(
        http(&d.ui, "POST", "/v1/voice/live", Some(bad)).await.0,
        400
    );

    // Speed and language reach the provider.
    let (status, _) = put_voice(
        &d,
        json!({"provider": "elevenlabs", "voice": "voice123", "speed": 1.1, "language": "DE"}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        http(&d.ui, "GET", "/v1/voice", None).await.1["language"],
        "de"
    );

    // Switched off: nothing speaks, listens or calls, settings and keys stay.
    let (status, voice) = put_voice(
        &d,
        json!({"provider": "elevenlabs", "voice": "voice123", "enabled": false}),
    )
    .await;
    assert_eq!((status, &voice["enabled"]), (200, &json!(false)));
    assert_eq!(voice["providers"][0]["has_key"], true);
    assert_eq!(speak(&d, "Hi").await.0, 409);
    let live = json!({"mode": "call", "sample_rate": 16000});
    assert_eq!(
        http(&d.ui, "POST", "/v1/voice/live", Some(live)).await.0,
        409
    );
    assert_eq!(http(&d.ui, "GET", "/v1/voice/catalog", None).await.0, 409);
    let call = json!({"agents": [agent]});
    assert_eq!(http(&d.ui, "PUT", "/v1/call", Some(call)).await.0, 409);
}

/// Messages queued for `agent` (not the ones it sent).
async fn received(d: &Daemon, agent: AgentId) -> Vec<Value> {
    let (_, msgs) = http(&d.ui, "GET", &format!("/v1/agents/{agent}/messages"), None).await;
    msgs.as_array()
        .unwrap()
        .iter()
        .filter(|m| m["recipient"] == agent.to_string())
        .cloned()
        .collect()
}

/// Says `text` in a fake runner's turn and ends it with `outcome`.
async fn answer_with(
    ws: &mut Ws,
    turn_id: String,
    text: &str,
    outcome: ferrobot_core::protocol::TurnOutcome,
) {
    use ferrobot_core::protocol::TurnEvent;
    let frames = [
        RunnerFrame::Event {
            turn_id: turn_id.clone(),
            event: TurnEvent::AssistantText { text: text.into() },
        },
        RunnerFrame::TurnDone {
            turn_id,
            outcome,
            session_id: None,
            result: None,
            error: None,
            steered: vec![],
        },
    ];
    for frame in frames {
        ws.send(Message::text(serde_json::to_string(&frame).unwrap()))
            .await
            .unwrap();
    }
}

async fn answer(ws: &mut Ws, turn_id: String, text: &str) {
    answer_with(
        ws,
        turn_id,
        text,
        ferrobot_core::protocol::TurnOutcome::Completed,
    )
    .await;
}

/// Takes a fake runner's next turn and says `text` in it; returns what its model got.
async fn say_turn(ws: &mut Ws, text: &str) -> String {
    let (turn_id, _, body) = next_turn(ws).await;
    answer(ws, turn_id, text).await;
    body
}

/// What was said in `room`: (speaker, words).
async fn room_lines(d: &Daemon, room: i64) -> Vec<(Value, String)> {
    let (status, room) = http(&d.ui, "GET", &format!("/v1/rooms/{room}"), None).await;
    assert_eq!(status, 200, "{room}");
    room["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["speaker"].clone(), l["body"].as_str().unwrap().to_owned()))
        .collect()
}

async fn lines_reach(d: &Daemon, room: i64, n: usize) -> Vec<(Value, String)> {
    eventually("the room's lines", Duration::from_secs(5), || async {
        let lines = room_lines(d, room).await;
        (lines.len() == n).then_some(lines)
    })
    .await
}

/// Room turns queued for `agent`; its chat leaves them out.
async fn room_mail(d: &Daemon, agent: AgentId) -> usize {
    let inbox = d.state.store.inbox(agent, 100).await.unwrap();
    inbox.iter().filter(|m| m.room_id.is_some()).count()
}

/// Waits until `agent` recorded `n` finished turns (their lines and hand-offs come first).
async fn turns_done(d: &Daemon, agent: AgentId, n: usize) {
    let id = agent.to_string();
    eventually("finished turns", Duration::from_secs(5), || async {
        let done = events(d, &id).await;
        (done.iter().filter(|e| e["kind"] == "turn_done").count() == n).then_some(())
    })
    .await;
}

async fn quiet(ws: &mut Ws) {
    let next = tokio::time::timeout(Duration::from_millis(300), ws.next()).await;
    assert!(next.is_err(), "no turn expected: {next:?}");
}

async fn invite(client: &McpClient, agent: &str, topic: &str) {
    let args = json!({"agent": agent, "topic": topic});
    let request = rmcp::model::CallToolRequestParams::new("invite_to_call")
        .with_arguments(args.as_object().cloned().unwrap());
    let result = client.call_tool(request).await.unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
}

#[tokio::test]
async fn calls_with_two_agents_move_into_a_room_where_agents_speak_one_at_a_time() {
    use ferrobot_core::protocol::TurnOutcome::Interrupted;
    use ferrobot_daemon::UiEvent;
    use rmcp::model::CallToolRequestParams;
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let mut ids = Vec::new();
    for name in ["ann", "bob", "cy", "dot", "eve"] {
        let agent = create(&d, json!({ "name": name })).await;
        ids.push(agent["id"].as_str().unwrap().parse::<AgentId>().unwrap());
    }
    let [ann, bob, cy, dot, eve] = ids[..] else {
        unreachable!()
    };
    let say = |body: &'static str| {
        let ui = d.ui.clone();
        async move {
            let (status, sent) = http(
                &ui,
                "POST",
                "/v1/call/messages",
                Some(json!({ "body": body })),
            )
            .await;
            assert_eq!(status, 202, "{sent}");
            sent.as_array().unwrap().clone()
        }
    };
    let ask = json!({"questions": [{"prompt": "Notes", "kind": "text"}]});

    // One on one: the call stays in ann's chat and opens no room; questions are asked out loud.
    let (status, started) = http(&d.ui, "PUT", "/v1/call", Some(json!({"agents": [ann]}))).await;
    assert_eq!(status, 200);
    assert_eq!(started, json!({"agents": [ann], "room_id": null}));
    let mcp = mcp_as(&d, backend.token("ann")).await;
    let refused = mcp
        .call_tool(
            CallToolRequestParams::new("ask_user")
                .with_arguments(ask.as_object().cloned().unwrap()),
        )
        .await;
    assert!(
        format!("{refused:?}").contains("ask out loud"),
        "{refused:?}"
    );
    assert_eq!(say("Hello ann").await[0]["room_id"], Value::Null);
    let mut ann_ws = connect_runner(&d.agents, &backend.token("ann")).await;
    let (ann_turn, _, body) = next_turn(&mut ann_ws).await;
    assert!(body.starts_with("(Voice call:"), "{body}");
    for part in [
        "Speak the operator's language.",
        "ask the operator out loud, never with ask_user",
        "(You host the call.",
        "(tell it to speak this call's language)",
    ] {
        assert!(body.contains(part), "{part}: {body}");
    }
    assert!(body.ends_with("The operator: Hello ann"), "{body}");
    assert_eq!(received(&d, ann).await.len(), 1, "in ann's chat");
    assert_eq!(http(&d.ui, "GET", "/v1/rooms", None).await.1, json!([]));

    // ann brings bob in during its turn: a room opens, and bob speaks after ann's announcement.
    invite(&mcp, "@bob", "Is the deploy done?").await;
    let (agents, room) = d.state.call_view().await;
    assert_eq!(agents, [ann, bob]);
    let room = room.expect("a room with the second agent");
    assert_eq!(
        http(&d.ui, "GET", "/v1/call", None).await.1["room_id"],
        room
    );
    let (_, rooms) = http(&d.ui, "GET", "/v1/rooms", None).await;
    assert_eq!(rooms[0]["members"], json!([ann, bob]));
    assert_eq!(rooms[0]["ended_at"], Value::Null);
    let topic = (
        json!({"kind": "agent", "id": ann}),
        "Is the deploy done?".into(),
    );
    assert_eq!(room_lines(&d, room).await, [topic]);
    let (_, transcript) = http(&d.ui, "GET", &format!("/v1/rooms/{room}"), None).await;
    assert_eq!(
        transcript["lines"][0]["invited"],
        json!(bob),
        "shown as an invitation"
    );
    assert!(
        d.state.invite_to_call(cy, dot, "x").await.is_err(),
        "cy is not in the call"
    );
    let mut bob_ws = connect_runner(&d.agents, &backend.token("bob")).await;
    quiet(&mut bob_ws).await;
    assert_eq!(room_mail(&d, bob).await, 0, "bob waits for ann's turn");
    answer(&mut ann_ws, ann_turn, "Bob joins us now.").await;
    let body = say_turn(&mut bob_ws, "Hi. Deploy is green.").await;
    assert!(!body.contains("You host"), "{body}");
    assert!(
        body.contains("@ann invited you into this call. Listen first: say at most a short hello"),
        "{body}"
    );
    assert!(body.ends_with("@ann: Is the deploy done?"), "{body}");
    assert!(
        received(&d, bob).await.is_empty(),
        "room turns stay out of chats"
    );

    // ann hears bob in a room turn and passes: no line, nobody else is due.
    let body = say_turn(&mut ann_ws, "Pass.").await;
    assert!(body.contains("(You host the call."), "{body}");
    assert!(body.ends_with("@bob: Hi. Deploy is green."), "{body}");
    turns_done(&d, ann, 2).await;
    let lines = room_lines(&d, room).await;
    assert_eq!(lines.len(), 2, "one-on-one words and passes make no line");
    assert_eq!(room_mail(&d, bob).await, 1);
    let ann_in = events(&d, &ann.to_string()).await;
    let ann_in: Vec<_> = ann_in
        .iter()
        .filter(|e| e["kind"] == "message_in")
        .collect();
    assert_eq!(ann_in.last().unwrap()["payload"]["room_id"], room);
    let unread = |agent: AgentId| {
        let ui = d.ui.clone();
        async move {
            http(&ui, "GET", &format!("/v1/agents/{agent}"), None)
                .await
                .1["unread"]
                .clone()
        }
    };
    assert_eq!(unread(ann).await, 1, "only the one-on-one turn");
    assert_eq!(unread(bob).await, 0, "room turns are not in the chat");

    // The operator: everyone is due, host first, one at a time, each hearing the one before.
    let sent = say("Thanks. What's next?").await;
    assert_eq!(sent.len(), 1);
    assert_eq!(
        (&sent[0]["recipient"], &sent[0]["room_id"]),
        (&json!(ann), &json!(room))
    );
    assert_eq!(received(&d, ann).await.len(), 1, "not in ann's chat");
    let (ann_turn, _, body) = next_turn(&mut ann_ws).await;
    assert!(
        body.ends_with("The operator: Thanks. What's next?"),
        "{body}"
    );
    quiet(&mut bob_ws).await;
    answer(&mut ann_ws, ann_turn, "a1").await;
    let body = say_turn(&mut bob_ws, "b1").await;
    assert!(
        body.ends_with("The operator: Thanks. What's next?\n@ann: a1"),
        "bob hears ann first: {body}"
    );
    assert!(say_turn(&mut ann_ws, "a2").await.ends_with("@bob: b1"));
    assert!(say_turn(&mut bob_ws, "b2").await.ends_with("@ann: a2"));
    assert!(say_turn(&mut ann_ws, "a3").await.ends_with("@bob: b2"));
    lines_reach(&d, room, 8).await;
    quiet(&mut ann_ws).await;
    quiet(&mut bob_ws).await;

    // The operator's words bring what waited; an interrupted turn waits for the operator.
    say("Go on.").await;
    assert!(
        say_turn(&mut ann_ws, "a4")
            .await
            .ends_with("The operator: Go on.")
    );
    let (bob_turn, _, body) = next_turn(&mut bob_ws).await;
    assert!(
        body.ends_with("@ann: a3\nThe operator: Go on.\n@ann: a4"),
        "{body}"
    );
    answer_with(&mut bob_ws, bob_turn, "cut off", Interrupted).await;
    turns_done(&d, bob, 4).await;
    assert_eq!(room_lines(&d, room).await.len(), 10, "interrupted: no line");
    quiet(&mut ann_ws).await;

    // A third agent joins while ann holds the floor: cy hears ann's announcement first.
    say("Ask cy about it.").await;
    let (ann_turn, _, _) = next_turn(&mut ann_ws).await;
    invite(&mcp, "@cy", "Can you check the logs?").await;
    let mut cy_ws = connect_runner(&d.agents, &backend.token("cy")).await;
    quiet(&mut cy_ws).await;
    answer(&mut ann_ws, ann_turn, "cy joins us.").await;
    let body = say_turn(&mut cy_ws, "(pass)").await;
    assert!(body.contains("@ann invited you into this call."), "{body}");
    assert!(
        body.ends_with("@ann: Can you check the logs?\n@ann: cy joins us."),
        "{body}"
    );
    say_turn(&mut bob_ws, "(pass)").await;
    lines_reach(&d, room, 13).await;

    // bob leaves after its goodbye; the host's goodbye ends the call, and the event names it.
    let left = call(
        &mcp_as(&d, backend.token("bob")).await,
        "hang_up",
        json!({}),
    )
    .await;
    assert_eq!(left["ends_call"], false);
    say("Bye all").await;
    say_turn(&mut ann_ws, "See you.").await;
    say_turn(&mut bob_ws, "Bye!").await;
    eventually("bob left", Duration::from_secs(5), || async {
        (d.state.call_view().await == (vec![ann, cy], Some(room))).then_some(())
    })
    .await;
    say_turn(&mut cy_ws, "Bye.").await;
    let (ann_turn, _, _) = next_turn(&mut ann_ws).await;
    assert_eq!(d.state.hang_up(ann).await, Ok(true));
    let mut ui = d.state.ui.subscribe();
    answer(&mut ann_ws, ann_turn, "Goodbye.").await;
    eventually("the call ended", Duration::from_secs(5), || async {
        (d.state.call_view().await == (vec![], None)).then_some(())
    })
    .await;
    let mut ended_by = None;
    while let Ok(event) = ui.try_recv() {
        if let UiEvent::Call {
            agents, hung_up, ..
        } = event
            && agents.is_empty()
        {
            ended_by = hung_up;
        }
    }
    assert_eq!(ended_by, Some(ann));
    let (_, transcript) = http(&d.ui, "GET", &format!("/v1/rooms/{room}"), None).await;
    assert_ne!(transcript["ended_at"], Value::Null, "{transcript}");
    assert_eq!(transcript["members"], json!([ann, bob, cy]));
    let words: Vec<_> = transcript["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["body"].clone())
        .collect();
    assert_eq!(
        &words[words.len() - 5..],
        ["Bye all", "See you.", "Bye!", "Bye.", "Goodbye."]
    );
    assert!(d.state.hang_up(bob).await.is_err(), "no call to hang up");
    quiet(&mut bob_ws).await;
    quiet(&mut cy_ws).await;

    // The operator hangs up: the running room turn is cut off and adds nothing; queued ones drop.
    let put = |agents: Vec<AgentId>| {
        let ui = d.ui.clone();
        async move { http(&ui, "PUT", "/v1/call", Some(json!({ "agents": agents }))).await }
    };
    let second = put(vec![ann, bob]).await.1["room_id"].as_i64().unwrap();
    say("Next topic.").await;
    let (ann_turn, _, _) = next_turn(&mut ann_ws).await;
    assert_eq!(put(vec![]).await.0, 200);
    match next_frame(&mut ann_ws).await {
        DaemonFrame::Interrupt { turn_id } => assert_eq!(turn_id, ann_turn),
        other => panic!("expected the turn cut off, got {other:?}"),
    }
    answer(&mut ann_ws, ann_turn, "late words").await;
    turns_done(&d, ann, 10).await;
    assert_eq!(room_lines(&d, second).await.len(), 1, "only the operator's");
    put(vec![dot, eve]).await;
    say("Anyone?").await;
    assert_eq!(
        room_mail(&d, dot).await,
        1,
        "dot's turn waits for its runner"
    );
    put(vec![]).await;
    let dot_mail = d.state.store.inbox(dot, 10).await.unwrap();
    assert!(
        dot_mail
            .iter()
            .all(|m| m.status != ferrobot_core::MailStatus::Pending),
        "{dot_mail:?}"
    );

    // At most four agents; an empty call ends it, and its room; questions are back.
    assert_eq!(put(vec![ann, bob, cy, dot, eve]).await.0, 400);
    let (status, full) = put(vec![ann, bob, cy, dot]).await;
    assert_eq!(status, 200);
    assert!(full["room_id"].as_i64().is_some(), "{full}");
    assert!(
        d.state.invite_to_call(ann, eve, "x").await.is_err(),
        "call is full"
    );
    assert_eq!(put(vec![]).await.0, 200);
    let (_, rooms) = http(&d.ui, "GET", "/v1/rooms", None).await;
    assert!(
        rooms
            .as_array()
            .unwrap()
            .iter()
            .all(|r| !r["ended_at"].is_null()),
        "{rooms}"
    );
    let (status, _) = http(
        &d.ui,
        "POST",
        "/v1/call/messages",
        Some(json!({"body": "hi"})),
    )
    .await;
    assert_eq!(status, 409);
    assert!(call(&mcp, "ask_user", ask).await["question_id"].is_i64());
}

#[tokio::test]
async fn message_limits_count_mentions_as_stored() {
    use rmcp::model::CallToolRequestParams;
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let ada: AgentId = create(&d, json!({"name": "ada"})).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    create(&d, json!({"name": "bob"})).await;
    let (status, _) = http(&d.ui, "PUT", "/v1/call", Some(json!({"agents": [ada]}))).await;
    assert_eq!(status, 200);
    let as_bob = mcp_as(&d, backend.token("bob")).await;
    let chat = format!("/v1/agents/{ada}/messages");

    // Each body is exactly at its limit; `@bob` grows into a `<@id>` token, `@zed` is no agent.
    let at_limit = |limit: usize, name: &str| format!("@{name} {}", "x".repeat(limit - 5));
    for (name, accepted) in [("zed", true), ("bob", false)] {
        let expected = if accepted { 202 } else { 400 };
        for (path, limit) in [(chat.as_str(), 32 * 1024), ("/v1/call/messages", 19316)] {
            let body = json!({"body": at_limit(limit, name)});
            let (status, _) = http(&d.ui, "POST", path, Some(body)).await;
            assert_eq!(status, expected, "{path} @{name}");
        }
        let body = at_limit(ferrobot_mcp::MAX_MESSAGE_BYTES, name);
        for (tool, field) in [("send_message", "body"), ("assign_task", "task")] {
            let mut args = serde_json::Map::new();
            args.insert("to".into(), json!("ada"));
            args.insert(field.into(), json!(body));
            let res = as_bob
                .call_tool(CallToolRequestParams::new(tool).with_arguments(args))
                .await;
            let ok = res.is_ok_and(|r| r.is_error != Some(true));
            assert_eq!(ok, accepted, "{tool} @{name}");
        }
    }
    let stored = received(&d, ada).await;
    assert_eq!(stored.len(), 4, "only the @zed bodies are stored");

    // A task that mentions the agent it creates fails before the operator is asked.
    let task = at_limit(ferrobot_mcp::MAX_MESSAGE_BYTES, "cal");
    let args = json!({"name": "cal", "description": "calendar", "task": task});
    let request = CallToolRequestParams::new("create_agent")
        .with_arguments(args.as_object().cloned().unwrap());
    let res = tokio::time::timeout(Duration::from_secs(5), as_bob.call_tool(request))
        .await
        .expect("rejected without an approval card");
    assert!(!res.is_ok_and(|r| r.is_error != Some(true)));
    let (_, all) = http(&d.ui, "GET", "/v1/agents", None).await;
    assert!(!all.to_string().contains("\"cal\""), "{all}");

    // With Auto mode on it fails before the agent is created.
    http(
        &d.ui,
        "PUT",
        "/v1/settings",
        Some(json!({"auto_mode": true})),
    )
    .await;
    let request = CallToolRequestParams::new("create_agent")
        .with_arguments(args.as_object().cloned().unwrap());
    let res = as_bob.call_tool(request).await;
    assert!(!res.is_ok_and(|r| r.is_error != Some(true)));
    let (_, all) = http(&d.ui, "GET", "/v1/agents", None).await;
    assert!(!all.to_string().contains("\"cal\""), "{all}");
}

/// Fake model API that streams usage like Anthropic and `OpenAI`, split mid-line as networks do.
/// Model `claude-hang` stalls after its first event; `claude-overloaded` ends in an error event.
async fn usage_upstream() -> String {
    fn sse(chunks: &'static [&'static str], hang: bool) -> axum::response::Response {
        let stream =
            futures_util::stream::iter(chunks.iter().copied().map(Ok::<_, std::io::Error>));
        let pings = futures_util::stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Some((Ok(": ping\n\n"), ()))
        });
        let body = if hang {
            Body::from_stream(stream.chain(pings))
        } else {
            Body::from_stream(stream)
        };
        ([("content-type", "text/event-stream")], body).into_response()
    }
    const BROKEN: &[&str] = &[
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12}}}\n\n",
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
    ];
    const ANTHROPIC: &[&str] = &[
        r#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":12,"cache_read_input_tokens":1000,"cache_creation_input_tokens":200,"output_tokens":1}}}

event: content_block_delta
data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"the answer"}}

event: message_delta
data: {"type":"message_"#,
        r#"delta","usage":{"output_tokens":30}}

"#,
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ];
    const OPENAI: &[&str] = &[
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}],\"usage\":null}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":7,",
        "\"prompt_tokens_details\":{\"cached_tokens\":60}}}\n\ndata: [DONE]\n\n",
    ];
    const WITH_COST: &[&str] = &[
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":900,\"completion_tokens\":50,\"cost\":0.0042}}\n\n",
        "data: [DONE]\n\n",
    ];
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new()
        .route(
            "/v1/messages",
            axum::routing::post(|body: String| async move {
                if body.contains("claude-hang") {
                    sse(&BROKEN[..1], true)
                } else if body.contains("claude-overloaded") {
                    sse(BROKEN, false)
                } else {
                    sse(ANTHROPIC, false)
                }
            }),
        )
        .route(
            "/v1/chat/completions",
            axum::routing::post(|body: String| async move {
                if body.contains("tencent/") {
                    sse(WITH_COST, false)
                } else {
                    sse(OPENAI, false)
                }
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn the_proxy_records_usage_per_call() {
    let upstream = usage_upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ferrobot.db");
    let backend = Arc::new(StatefulBackend::default());
    let d = start_on(
        ferrobot_store::Store::open(&db).unwrap(),
        backend.clone(),
        None,
        "127.0.0.1:0",
        |s| {
            s.driver = "claude".into();
            s.anthropic_api_key = Some("sk-real-secret".into());
            s.anthropic_upstream.clone_from(&upstream);
        },
    )
    .await;
    create(&d, json!({"name": "ann"})).await;
    let (_, openai) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(
            json!({"name": "oai", "kind": "openai", "base_url": format!("{upstream}/v1"),
                    "default_model": "gpt-5-mini", "api_key": "sk-openai"}),
        ),
    )
    .await;
    create(&d, json!({"name": "oli", "provider_id": openai["id"]})).await;

    let (status, _, out) = http_raw(
        &d.agents,
        "POST",
        "/model/v1/messages",
        &[("x-api-key", &backend.token("ann"))],
        br#"{"model":"claude-sonnet-4-5-20250929","stream":true}"#,
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        String::from_utf8(out).unwrap().contains("the answer"),
        "bytes pass unchanged"
    );
    let bearer = format!("Bearer {}", backend.token("oli"));
    let body = br#"{"model":"gpt-5-mini","stream":true,"stream_options":{"include_usage":true}}"#;
    let (status, _, _) = http_raw(
        &d.agents,
        "POST",
        "/model/chat/completions",
        &[("authorization", &bearer)],
        body,
    )
    .await;
    assert_eq!(status, 200);
    let (status, _, _) = http_raw(
        &d.agents,
        "POST",
        "/model/v1beta/models/sneaky:generateContent",
        &[("x-api-key", &backend.token("ann"))],
        b"",
    )
    .await;
    assert_eq!(status, 404, "a refused call keeps no model from its path");

    let conn = rusqlite::Connection::open(&db).unwrap();
    let rows = eventually("three usage rows", Duration::from_secs(5), || {
        let mut stmt = conn
            .prepare("SELECT agent_name, model, status FROM usage ORDER BY agent_name, status")
            .unwrap();
        let rows: Vec<(String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        std::future::ready((rows.len() == 3).then_some(rows))
    })
    .await;
    let row = |n: &str, m: &str, s: &str| (n.to_owned(), m.to_owned(), s.to_owned());
    assert_eq!(
        rows,
        [
            row("ann", "claude-sonnet-4-5-20250929", "ok"),
            row("ann", "unknown", "refused"),
            row("oli", "gpt-5-mini", "ok"),
        ]
    );

    let (status, report) = http(&d.ui, "GET", "/v1/usage?group_by=model", None).await;
    assert_eq!(status, 200, "{report}");
    let groups = report["groups"].as_array().unwrap();
    let claude = groups
        .iter()
        .find(|g| g["key"] == "anthropic/claude-sonnet-4-5-20250929")
        .unwrap();
    let counts = |g: &Value| {
        [
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
        ]
        .map(|k| g[k].as_i64())
    };
    assert_eq!(counts(claude), [Some(12), Some(30), Some(1000), Some(200)]);
    // 12 * 3 + 30 * 15 + 1000 * 0.30 + 200 * 3.75 USD per 1M tokens
    assert!(
        (claude["cost_usd"].as_f64().unwrap() - 0.001_536).abs() < 1e-12,
        "{claude}"
    );
    let gpt = groups
        .iter()
        .find(|g| g["key"] == "openai/gpt-5-mini")
        .unwrap();
    assert_eq!(
        counts(gpt),
        [Some(40), Some(7), Some(60), Some(0)],
        "cached tokens leave the input count"
    );
    assert_eq!(report["totals"]["calls"], 3);
    assert_eq!(report["totals"]["not_tracked_calls"], 0);
}

#[tokio::test]
async fn usage_of_a_broken_stream_is_an_error() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let upstream = usage_upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ferrobot.db");
    let backend = Arc::new(StatefulBackend::default());
    let d = start_on(
        ferrobot_store::Store::open(&db).unwrap(),
        backend.clone(),
        None,
        "127.0.0.1:0",
        |s| {
            s.driver = "claude".into();
            s.anthropic_api_key = Some("sk-real-secret".into());
            s.anthropic_upstream.clone_from(&upstream);
        },
    )
    .await;
    create(&d, json!({"name": "ann"})).await;
    let key = backend.token("ann");

    let (status, _, _) = http_raw(
        &d.agents,
        "POST",
        "/model/v1/messages",
        &[("x-api-key", &key)],
        br#"{"model":"claude-overloaded","stream":true}"#,
    )
    .await;
    assert_eq!(status, 200);

    // The agent hangs up after the first event while the upstream keeps streaming.
    let body = br#"{"model":"claude-hang","stream":true}"#;
    let mut s = tokio::net::TcpStream::connect(&d.agents).await.unwrap();
    let head = format!(
        "POST /model/v1/messages HTTP/1.1\r\nHost: x\r\nx-api-key: {key}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
    let mut seen = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !String::from_utf8_lossy(&seen).contains("message_start") {
            let mut buf = [0; 1024];
            let n = s.read(&mut buf).await.unwrap();
            assert!(n > 0, "stream closed early");
            seen.extend_from_slice(&buf[..n]);
        }
    })
    .await
    .unwrap();
    drop(s);

    let conn = rusqlite::Connection::open(&db).unwrap();
    let rows = eventually("two usage rows", Duration::from_secs(5), || {
        let mut stmt = conn
            .prepare("SELECT model, status, input_tokens FROM usage ORDER BY model")
            .unwrap();
        let rows: Vec<(String, String, Option<i64>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        std::future::ready((rows.len() == 2).then_some(rows))
    })
    .await;
    let row = |m: &str| (m.to_owned(), "error".to_owned(), Some(12));
    assert_eq!(
        rows,
        [row("claude-hang"), row("claude-overloaded")],
        "counts seen before the break are kept"
    );
}

#[tokio::test]
async fn usage_filters_groups_and_price_overrides() {
    use ferrobot_store::{Tokens, UsageRecord, UsageStatus};
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let (amy, bob) = (AgentId::new(), AgentId::new());
    let row = |at: &str, agent: AgentId, kind: &str, model: &str, input: Option<i64>| UsageRecord {
        at: time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339)
            .unwrap(),
        agent_id: agent,
        agent_name: if agent == amy { "amy" } else { "bob" }.into(),
        provider_id: None,
        provider_kind: kind.into(),
        model: model.into(),
        tokens: Tokens {
            input,
            output: input,
            cache_read: None,
            cache_write: None,
        },
        cost_usd: None,
        latency_ms: 5,
        status: UsageStatus::Ok,
        turn_id: None,
    };
    for r in [
        row(
            "2026-11-18T10:00:00Z",
            amy,
            "anthropic",
            "claude-opus-5",
            Some(1_000_000),
        ),
        row(
            "2026-11-19T10:00:00Z",
            amy,
            "anthropic",
            "house-model",
            Some(10),
        ),
        row("2026-11-19T12:00:00Z", bob, "ollama", "llama9", Some(50)),
        row(
            "2026-11-19T13:00:00Z",
            bob,
            "bedrock",
            "anthropic.claude-opus-5",
            None,
        ),
    ] {
        d.state.store.insert_usage(r).await.unwrap();
    }

    let (status, all) = http(&d.ui, "GET", "/v1/usage", None).await;
    assert_eq!(status, 200, "{all}");
    let days: Vec<_> = all["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["key"].clone())
        .collect();
    assert_eq!(days, [json!("2026-11-18"), json!("2026-11-19")]);
    assert_eq!(all["totals"]["calls"], 4);
    assert_eq!(all["totals"]["unknown_cost_calls"], 1);
    assert_eq!(all["totals"]["not_tracked_calls"], 1);
    let total = all["totals"]["cost_usd"].as_f64().unwrap();
    assert!(
        (total - 30.0).abs() < 1e-9,
        "opus 5 at 5 + 25 per 1M: {total}"
    );

    // A browser's toISOString() sends milliseconds.
    let day = "from=2026-11-19T00:00:00.000Z&to=2026-11-20T00:00:00Z";
    let (_, by_agent) = http(
        &d.ui,
        "GET",
        &format!("/v1/usage?{day}&group_by=agent"),
        None,
    )
    .await;
    let groups = by_agent["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "{by_agent}");
    let amy_day = groups.iter().find(|g| g["label"] == "amy").unwrap();
    assert_eq!(
        amy_day["cost_usd"],
        Value::Null,
        "only an unknown model that day"
    );
    let bob_day = groups.iter().find(|g| g["label"] == "bob").unwrap();
    assert_eq!(bob_day["cost_usd"], json!(0.0), "ollama is free");
    assert_eq!(bob_day["not_tracked_calls"], 1);
    let (_, only_bob) = http(
        &d.ui,
        "GET",
        &format!("/v1/usage?{day}&agent={bob}&group_by=model"),
        None,
    )
    .await;
    assert_eq!(only_bob["totals"]["calls"], 2, "{only_bob}");

    for bad in [
        "from=yesterday",
        "group_by=week",
        "agent=nope",
        "provider=x",
        "from=2026-11-20T00:00:00Z&to=2026-11-19T00:00:00Z",
    ] {
        let (status, err) = http(&d.ui, "GET", &format!("/v1/usage?{bad}"), None).await;
        assert_eq!(status, 400, "{bad}: {err}");
    }

    let set = json!([{"provider_kind": "anthropic", "model": "house-model", "input": 1.0, "output": 1.0}]);
    let (status, prices) = http(&d.ui, "PUT", "/v1/usage/prices", Some(set)).await;
    assert_eq!(status, 200, "{prices}");
    let prices = prices.as_array().unwrap();
    let house = prices.iter().find(|p| p["model"] == "house-model").unwrap();
    assert_eq!(house["source"], Value::Null, "the operator's own price");
    assert!(
        prices
            .iter()
            .any(|p| p["model"] == "claude-opus-5" && p["source"].is_string())
    );
    let (_, report) = http(&d.ui, "GET", &format!("/v1/usage?{day}&agent={amy}"), None).await;
    let cost = report["totals"]["cost_usd"].as_f64().unwrap();
    assert!((cost - 0.000_02).abs() < 1e-12, "{report}");
    let negative =
        json!([{"provider_kind": "anthropic", "model": "m", "input": -1.0, "output": 1.0}]);
    let (status, _) = http(&d.ui, "PUT", "/v1/usage/prices", Some(negative)).await;
    assert_eq!(status, 400);

    let pid = uuid::Uuid::new_v4();
    let mut keyed = row("2026-11-17T10:00:00Z", amy, "openai", "gpt-5", Some(1));
    keyed.provider_id = Some(pid);
    d.state.store.insert_usage(keyed).await.unwrap();
    let braced = format!(
        "/v1/usage?provider=%7B{}%7D",
        pid.to_string().to_uppercase()
    );
    let (status, by_provider) = http(&d.ui, "GET", &braced, None).await;
    assert_eq!(status, 200, "{by_provider}");
    assert_eq!(by_provider["totals"]["calls"], 1, "{by_provider}");
}

#[tokio::test]
async fn usage_days_follow_the_daemon_time_zone() {
    use ferrobot_store::{Tokens, UsageRecord, UsageStatus};
    let d = start_with(
        Arc::new(StatefulBackend::default()),
        None,
        "127.0.0.1:0",
        |s| s.default_timezone = "Europe/Berlin".into(),
    )
    .await;
    let (amy, bob) = (AgentId::new(), AgentId::new());
    let row = |at: &str, agent: AgentId, kind: &str, model: &str, tokens: i64| UsageRecord {
        at: time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339)
            .unwrap(),
        agent_id: agent,
        agent_name: if agent == amy { "amy" } else { "bob" }.into(),
        provider_id: None,
        provider_kind: kind.into(),
        model: model.into(),
        tokens: Tokens {
            input: Some(tokens),
            output: Some(tokens),
            cache_read: None,
            cache_write: None,
        },
        cost_usd: None,
        latency_ms: 5,
        status: UsageStatus::Ok,
        turn_id: None,
    };
    // 23:30 and 00:30 in Berlin: one UTC day, two local days.
    for r in [
        row(
            "2026-11-18T22:30:00Z",
            amy,
            "anthropic",
            "claude-opus-5",
            1_000_000,
        ),
        row(
            "2026-11-18T23:30:00Z",
            amy,
            "anthropic",
            "claude-opus-5",
            1_000_000,
        ),
        row("2026-11-19T10:00:00Z", bob, "ollama", "llama9", 50),
    ] {
        d.state.store.insert_usage(r).await.unwrap();
    }

    let (status, report) = http(&d.ui, "GET", "/v1/usage?group_by=agent&daily=true", None).await;
    assert_eq!(status, 200, "{report}");
    assert_eq!(report["timezone"], "Europe/Berlin");
    let days = |label: &str| -> Vec<(String, f64)> {
        let group = report["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["label"] == label)
            .unwrap();
        group["days"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d["date"].as_str().unwrap().to_owned(),
                    d["cost_usd"].as_f64().unwrap(),
                )
            })
            .collect()
    };
    assert_eq!(
        days("amy"),
        [
            ("2026-11-18".to_owned(), 30.0),
            ("2026-11-19".to_owned(), 30.0)
        ]
    );
    assert_eq!(days("bob"), [("2026-11-19".to_owned(), 0.0)]);

    let (_, plain) = http(&d.ui, "GET", "/v1/usage?group_by=agent", None).await;
    assert!(plain.get("timezone").is_none(), "{plain}");
    assert!(plain["groups"][0].get("days").is_none(), "{plain}");
    let (_, utc) = http(&d.ui, "GET", "/v1/usage", None).await;
    assert_eq!(utc["groups"][0]["key"], "2026-11-18", "{utc}");
    assert_eq!(utc["groups"][0]["calls"], 2, "group_by=day stays UTC");
}

#[tokio::test]
async fn usage_totals_add_up_in_every_grouping() {
    use ferrobot_store::{Tokens, UsageRecord, UsageStatus};
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let (amy, bob) = (AgentId::new(), AgentId::new());
    let row =
        |at: &str, agent: AgentId, model: &str, output: i64, cache_read: Option<i64>| UsageRecord {
            at: time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339)
                .unwrap(),
            agent_id: agent,
            agent_name: if agent == amy { "amy" } else { "bob" }.into(),
            provider_id: None,
            provider_kind: "anthropic".into(),
            model: model.into(),
            tokens: Tokens {
                input: Some(1_000_000),
                output: Some(output),
                cache_read,
                cache_write: None,
            },
            cost_usd: None,
            latency_ms: 5,
            status: UsageStatus::Ok,
            turn_id: None,
        };
    for r in [
        row("2026-11-19T10:00:00Z", amy, "house-model", 1_000_000, None),
        // Cache reads, but the house price has no cache rate: only this call is unknown.
        row("2026-11-19T10:05:00Z", amy, "house-model", 0, Some(500)),
        row("2026-11-19T11:00:00Z", bob, "house-model", 1_000_000, None),
        row(
            "2026-11-20T09:00:00Z",
            bob,
            "claude-opus-5",
            1_000_000,
            Some(1_000_000),
        ),
    ] {
        d.state.store.insert_usage(r).await.unwrap();
    }
    // What OpenRouter reported it charged counts as is, in every grouping.
    let charged = UsageRecord {
        provider_kind: "openrouter".into(),
        model: "tencent/hy4-flash".into(),
        cost_usd: Some(0.25),
        ..row("2026-11-20T09:30:00Z", amy, "-", 10, None)
    };
    d.state.store.insert_usage(charged).await.unwrap();
    let house = json!([{"provider_kind": "anthropic", "model": "house-model", "input": 1.0, "output": 1.0}]);
    let (status, _) = http(&d.ui, "PUT", "/v1/usage/prices", Some(house)).await;
    assert_eq!(status, 200);

    let sum = |items: &Value| -> (f64, i64) {
        items
            .as_array()
            .unwrap()
            .iter()
            .fold((0.0, 0), |(c, u), g| {
                (
                    c + g["cost_usd"].as_f64().unwrap_or(0.0),
                    u + g["unknown_cost_calls"].as_i64().unwrap(),
                )
            })
    };
    let (_, card) = http(&d.ui, "GET", "/v1/usage", None).await;
    let card = (
        card["totals"]["cost_usd"].as_f64().unwrap(),
        card["totals"]["unknown_cost_calls"].as_i64().unwrap(),
    );
    assert_eq!(card.1, 1, "only the cache-reading call is unknown");
    let close = |a: (f64, i64), b: (f64, i64), what: &str| {
        assert!(
            (a.0 - b.0).abs() < 1e-9 && a.1 == b.1,
            "{what}: {a:?} vs {b:?}"
        );
    };
    for by in ["day", "agent", "provider", "model"] {
        for daily in ["", "&daily=true"] {
            let path = format!("/v1/usage?group_by={by}{daily}");
            let (status, report) = http(&d.ui, "GET", &path, None).await;
            assert_eq!(status, 200, "{report}");
            let totals = &report["totals"];
            let totals = (
                totals["cost_usd"].as_f64().unwrap(),
                totals["unknown_cost_calls"].as_i64().unwrap(),
            );
            close(totals, card, &format!("{path} totals vs the card"));
            close(
                sum(&report["groups"]),
                card,
                &format!("{path} rows vs the card"),
            );
            for g in report["groups"].as_array().unwrap() {
                if !daily.is_empty() {
                    let own = (
                        g["cost_usd"].as_f64().unwrap_or(0.0),
                        g["unknown_cost_calls"].as_i64().unwrap(),
                    );
                    close(
                        sum(&g["days"]),
                        own,
                        &format!("{path} days of {}", g["key"]),
                    );
                }
            }
        }
    }
    let (_, models) = http(&d.ui, "GET", "/v1/usage?group_by=model", None).await;
    let charged = models["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["label"] == "tencent/hy4-flash")
        .unwrap();
    assert_eq!(charged["cost_usd"], 0.25, "{charged}");
    let house = models["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["label"] == "house-model")
        .unwrap();
    close(
        (
            house["cost_usd"].as_f64().unwrap(),
            house["unknown_cost_calls"].as_i64().unwrap(),
        ),
        (4.0, 1),
        "two priced house calls at 1 + 1 per 1M",
    );
}

#[tokio::test]
async fn agents_get_connectors_through_a_card_whose_token_they_never_see() {
    use rmcp::model::CallToolRequestParams;
    const TOKEN: &str = "tok-zebra-4471";
    let (url, seen) = upstream_with(TOKEN, |_| axum::Router::new()).await;
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let ann = create(&d, json!({"name": "ann"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Auto mode does not skip a connector card.
    let auto = Some(json!({"auto_mode": true}));
    assert_eq!(http(&d.ui, "PUT", "/v1/settings", auto).await.0, 200);
    let mut ui = d.state.ui.subscribe();

    let as_ann = mcp_as(&d, backend.token("ann")).await;
    let args = json!({"name": "gh", "url": url, "auth": "token", "reason": "Read the issues"});
    let asked = call(&as_ann, "add_connector", args).await;
    let qid = asked["question_id"].as_i64().unwrap();
    assert_eq!(
        http(&d.ui, "GET", "/v1/connectors", None).await.1,
        json!([])
    );
    let card = events(&d, &ann)
        .await
        .into_iter()
        .find(|e| e["id"] == qid)
        .unwrap();
    let questions = card["payload"]["card"]["questions"].as_array().unwrap();
    let prompt = questions[0]["prompt"].as_str().unwrap();
    assert!(prompt.contains(&url), "{prompt}");
    assert!(prompt.contains("Settings → Vault"), "{prompt}");
    assert_eq!(questions.len(), 1, "no card asks for a key");

    let path = format!("/v1/agents/{ann}/questions/{qid}/answer");
    let approve = json!({"answers": [{"question_id": "q1", "values": ["approve"]}]});
    let (status, answered) = http(&d.ui, "POST", &path, Some(approve.clone())).await;
    assert_eq!(status, 202, "{answered}");
    let (status, _) = http(&d.ui, "POST", &path, Some(approve)).await;
    assert_eq!(status, 409, "answered once");

    // ann has the connector; its tools work once the operator puts the token in Settings.
    let (_, ids) = http(&d.ui, "GET", &format!("/v1/agents/{ann}/connectors"), None).await;
    assert_eq!(ids.as_array().unwrap().len(), 1);
    let gh_tools = || async {
        mcp_as(&d, backend.token("ann"))
            .await
            .list_all_tools()
            .await
            .unwrap()
            .iter()
            .any(|t| t.name == "gh__list_agents")
    };
    assert!(!gh_tools().await, "no token yet");
    let id = http(&d.ui, "GET", "/v1/connectors", None).await.1[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let set = Some(json!({ "token": TOKEN }));
    let (status, _) = http(&d.ui, "PATCH", &format!("/v1/connectors/{id}"), set).await;
    assert_eq!(status, 200);
    assert!(gh_tools().await);
    assert!(seen.lock().unwrap().contains(&format!("Bearer {TOKEN}")));
    let mail_now = mail(&d, &ann).await;
    assert!(
        mail_now.iter().any(|m| {
            let body = m["body"].as_str().unwrap();
            body.contains("added the connector \"gh\"") && body.contains("Settings → Vault")
        }),
        "{mail_now:?}"
    );

    // ...and nowhere else: not to the agent, an event, a message or the UI stream.
    let mut seen_by_others = vec![
        asked.to_string(),
        answered.to_string(),
        Value::Array(events(&d, &ann).await).to_string(),
        Value::Array(mail_now).to_string(),
    ];
    loop {
        match ui.try_recv() {
            Ok(event) => seen_by_others.push(serde_json::to_string(&event).unwrap()),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(e) => panic!("ui stream: {e}"),
        }
    }
    for text in seen_by_others {
        assert!(!text.contains(TOKEN), "{text}");
    }

    // Agents cannot put a secret field on their own cards, nor reuse a name.
    let secret_card = json!({"questions": [{"prompt": "Your key?", "kind": "secret"}]});
    let request = CallToolRequestParams::new("ask_user")
        .with_arguments(secret_card.as_object().cloned().unwrap());
    assert!(as_ann.call_tool(request).await.is_err());
    let taken = json!({"name": "gh", "url": url, "auth": "none"});
    let request = CallToolRequestParams::new("add_connector")
        .with_arguments(taken.as_object().cloned().unwrap());
    assert!(as_ann.call_tool(request).await.is_err());

    // Declined: nothing is created and ann hears so.
    let args = json!({"name": "docs", "url": url, "auth": "none"});
    let qid = call(&as_ann, "add_connector", args).await["question_id"]
        .as_i64()
        .unwrap();
    assert_eq!(decide(&d, &ann, qid, "decline").await.0, 202);
    let (_, all) = http(&d.ui, "GET", "/v1/connectors", None).await;
    assert_eq!(all.as_array().unwrap().len(), 1, "{all}");
    assert!(mail(&d, &ann).await.iter().any(|m| {
        m["body"]
            .as_str()
            .unwrap()
            .contains("declined adding the connector \"docs\"")
    }));
}

#[tokio::test]
async fn voice_keys_change_without_switching_the_provider() {
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let (status, _) = put_voice(&d, json!({"provider": "elevenlabs"})).await;
    assert_eq!(status, 200);
    let key = |provider: &str, value: Value| {
        let path = format!("/v1/voice/keys/{provider}");
        let d = &d;
        async move { http(&d.ui, "PUT", &path, Some(json!({ "api_key": value }))).await }
    };
    let (status, voice) = key("openrouter", json!("or-voice-key")).await;
    assert_eq!(status, 200, "{voice}");
    assert_eq!(voice["provider"], "elevenlabs");
    assert_eq!(voice["providers"][1]["has_key"], true);
    assert!(!voice.to_string().contains("or-voice-key"));
    let (_, voice) = key("openrouter", Value::Null).await;
    assert_eq!(voice["providers"][1]["has_key"], false);
    assert_eq!(key("nope", json!("x")).await.0, 404);
}

#[tokio::test]
async fn openrouter_calls_cost_what_openrouter_reports() {
    let upstream = usage_upstream().await;
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    for (name, kind) in [("or", "openrouter"), ("oai", "openai")] {
        let body = json!({"name": name, "kind": kind, "base_url": format!("{upstream}/v1"),
                          "api_key": "k", "default_model": "tencent/hy4-flash"});
        let (status, p) = http(&d.ui, "POST", "/v1/providers", Some(body)).await;
        assert_eq!(status, 201, "{p}");
        let agent = if kind == "openrouter" { "ora" } else { "oli" };
        create(&d, json!({"name": agent, "provider_id": p["id"]})).await;
    }
    for agent in ["ora", "oli"] {
        let bearer = format!("Bearer {}", backend.token(agent));
        let body = br#"{"model":"tencent/hy4-flash","stream":true}"#;
        let (status, _, _) = http_raw(
            &d.agents,
            "POST",
            "/model/chat/completions",
            &[("authorization", &bearer)],
            body,
        )
        .await;
        assert_eq!(status, 200);
    }
    let report = eventually("two calls", Duration::from_secs(5), || async {
        let (_, r) = http(&d.ui, "GET", "/v1/usage?group_by=agent", None).await;
        (r["totals"]["calls"] == 2).then_some(r)
    })
    .await;
    let group = |name: &str| {
        report["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["label"] == name)
            .unwrap()
            .clone()
    };
    let ora = group("ora");
    assert_eq!(
        ora["cost_usd"], 0.0042,
        "the charge, though no price is listed"
    );
    assert_eq!(ora["unknown_cost_calls"], 0);
    let oli = group("oli");
    assert_eq!(
        oli["cost_usd"],
        Value::Null,
        "only OpenRouter's word counts"
    );
    assert_eq!(oli["unknown_cost_calls"], 1);
    assert_eq!(report["totals"]["cost_usd"], 0.0042);
}

#[tokio::test]
async fn agents_create_routines_with_approval_or_in_auto_mode() {
    use ferrobot_mcp::{RoutineRequest, RoutineRequestOutcome};
    let request = |name: &str, schedule: &str| RoutineRequest {
        name: name.into(),
        schedule: schedule.into(),
        prompt: "Summarise yesterday's mail".into(),
    };
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let ada = create(&d, json!({"name": "ada"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id: AgentId = ada.parse().unwrap();
    let routines = || {
        let path = format!("/v1/agents/{ada}/routines");
        let ui = d.ui.clone();
        async move {
            let (_, list) = http(&ui, "GET", &path, None).await;
            list.as_array()
                .unwrap()
                .iter()
                .map(|r| r["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        }
    };
    let said = |text: &'static str| {
        let ada = ada.clone();
        let d = &d;
        async move {
            mail(d, &ada)
                .await
                .iter()
                .any(|m| m["body"].as_str().unwrap().contains(text))
        }
    };

    let RoutineRequestOutcome::Pending { question_id } = d
        .state
        .request_routine(id, request("digest", "0 9 * * 1-5"))
        .await
        .unwrap()
    else {
        panic!("expected a card");
    };
    assert!(
        routines().await.is_empty(),
        "nothing before the operator answers"
    );
    let card = events(&d, &ada)
        .await
        .into_iter()
        .find(|e| e["id"] == question_id)
        .unwrap();
    assert_eq!(card["payload"]["approval"]["kind"], "create_routine");
    let prompt = card["payload"]["card"]["questions"][0]["prompt"]
        .as_str()
        .unwrap();
    assert!(
        prompt.contains("At 09:00") && prompt.contains("Friday (UTC)"),
        "{prompt}"
    );
    assert_eq!(decide(&d, &ada, question_id, "approve").await.0, 202);
    assert_eq!(routines().await, ["digest"]);
    assert!(said("approved your routine \"digest\"").await);
    assert_eq!(decide(&d, &ada, question_id, "approve").await.0, 409);

    let err = d
        .state
        .request_routine(id, request("digest", "@daily"))
        .await
        .unwrap_err();
    assert!(err.contains("already"), "{err}");
    let err = d
        .state
        .request_routine(id, request("bad", "every morning"))
        .await
        .unwrap_err();
    assert_ne!(err, "", "an invalid schedule is refused");

    let RoutineRequestOutcome::Pending { question_id } = d
        .state
        .request_routine(id, request("weekly", "@weekly"))
        .await
        .unwrap()
    else {
        panic!("expected a card");
    };
    assert_eq!(decide(&d, &ada, question_id, "decline").await.0, 202);
    assert_eq!(routines().await, ["digest"]);
    assert!(said("declined your routine \"weekly\"").await);

    let auto = Some(json!({"auto_mode": true}));
    assert_eq!(http(&d.ui, "PUT", "/v1/settings", auto).await.0, 200);
    let outcome = d
        .state
        .request_routine(id, request("hourly", "@hourly"))
        .await
        .unwrap();
    assert!(
        matches!(outcome, RoutineRequestOutcome::Created { .. }),
        "{outcome:?}"
    );
    let mut names = routines().await;
    names.sort();
    assert_eq!(names, ["digest", "hourly"]);
}

#[tokio::test]
async fn agents_change_the_tools_of_agents_they_created() {
    use ferrobot_core::ToolGroup::{self, CreateAgents, Memory, Shell, Team, Web};
    use ferrobot_mcp::{AgentRequest, AgentRequestOutcome, AgentToolsOutcome, AgentToolsRequest};
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let auto = |on: bool| {
        let ui = d.ui.clone();
        async move {
            let body = Some(json!({"auto_mode": on}));
            assert_eq!(http(&ui, "PUT", "/v1/settings", body).await.0, 200);
        }
    };
    let ada = create(
        &d,
        json!({"name": "ada", "tools": ["team", "memory", "create_agents", "shell"]}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ada_id: AgentId = ada.parse().unwrap();
    let bob: AgentId = create(&d, json!({"name": "bob"})).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let gone = "http://127.0.0.1:1/mcp";
    let gh = add_connector(&d, json!({"name": "github", "url": gone})).await["id"].clone();
    let db = add_connector(&d, json!({"name": "db", "url": gone})).await["id"].clone();
    let path = format!("/v1/agents/{ada}/connectors");
    let mine = Some(json!([gh, db]));
    assert_eq!(http(&d.ui, "PUT", &path, mine).await.0, 200);

    auto(true).await;
    let AgentRequestOutcome::Created { id: scout_id, .. } = d
        .state
        .request_agent(
            ada_id,
            AgentRequest {
                tools: Some(vec![Team, Memory, Shell]),
                connectors: Some(vec!["github".into()]),
                ..agent_request("scout")
            },
        )
        .await
        .unwrap()
    else {
        panic!("expected created");
    };
    auto(false).await;
    let scout = scout_id.to_string();
    let connectors = || {
        let ui = d.ui.clone();
        let path = format!("/v1/agents/{scout}/connectors");
        async move { http(&ui, "GET", &path, None).await.1 }
    };
    let req = |agent: AgentId, tools: Option<Vec<ToolGroup>>, connectors: Option<&[&str]>| {
        AgentToolsRequest {
            agent,
            tools,
            connectors: connectors.map(|c| c.iter().map(|&n| n.to_owned()).collect()),
        }
    };

    // Refused before anything changes or the operator is asked.
    for (requester, request, error) in [
        (
            ada_id,
            req(bob, Some(vec![Team]), None),
            "agents you created",
        ),
        (
            scout_id,
            req(ada_id, Some(vec![Team]), None),
            "agents you created",
        ),
        (
            ada_id,
            req(ada_id, Some(vec![Team]), None),
            "your own tools",
        ),
        (ada_id, req(scout_id, None, None), "give tools"),
        (
            ada_id,
            req(scout_id, Some(vec![Team, Web]), None),
            "you do not have: web",
        ),
        (
            ada_id,
            req(scout_id, None, Some(&["slack"])),
            "no connector slack",
        ),
    ] {
        let err = d
            .state
            .set_agent_tools(requester, request)
            .await
            .unwrap_err();
        assert!(err.contains(error), "{err}");
    }
    assert_eq!(
        a_tools(&d, &scout).await,
        json!(["team", "memory", "shell"])
    );
    assert!(
        !events(&d, &ada)
            .await
            .iter()
            .any(|e| e["kind"] == "question"),
        "no card"
    );

    // Narrowing, through the MCP tool, applies at once and reaches the runner.
    let mut ws = connect_runner(&d.agents, &backend.token("scout")).await;
    let client = mcp_as(&d, backend.token("ada")).await;
    let args = json!({"agent": "@scout", "tools": ["team"], "connectors": []});
    let result = call(&client, "set_agent_tools", args).await;
    assert_eq!(result["status"], "applied", "{result}");
    let DaemonFrame::Configure { tools, .. } = next_frame(&mut ws).await else {
        panic!("expected configure");
    };
    assert_eq!(tools, [Team]);
    assert_eq!(a_tools(&d, &scout).await, json!(["team"]));
    assert_eq!(connectors().await, json!([]));

    // Anything new needs the card: decline changes nothing, approve applies.
    let wider = || req(scout_id, Some(vec![Team, Memory]), Some(&["db"]));
    let AgentToolsOutcome::Pending { question_id } =
        d.state.set_agent_tools(ada_id, wider()).await.unwrap()
    else {
        panic!("expected a card");
    };
    let card = events(&d, &ada)
        .await
        .into_iter()
        .find(|e| e["id"] == question_id)
        .unwrap();
    assert_eq!(card["payload"]["approval"]["kind"], "set_agent_tools");
    let prompt = card["payload"]["card"]["questions"][0]["prompt"]
        .as_str()
        .unwrap();
    assert!(prompt.contains("New for it: memory, db"), "{prompt}");
    assert_eq!(a_tools(&d, &scout).await, json!(["team"]));
    assert_eq!(decide(&d, &ada, question_id, "decline").await.0, 202);
    assert_eq!(a_tools(&d, &scout).await, json!(["team"]));
    assert_eq!(connectors().await, json!([]));
    let said = |text: &'static str| {
        let ada = ada.clone();
        let d = &d;
        async move {
            mail(d, &ada)
                .await
                .iter()
                .any(|m| m["body"].as_str().unwrap().contains(text))
        }
    };
    assert!(said("declined the new tools for scout").await);

    let AgentToolsOutcome::Pending { question_id } =
        d.state.set_agent_tools(ada_id, wider()).await.unwrap()
    else {
        panic!("expected a card");
    };
    assert_eq!(decide(&d, &ada, question_id, "approve").await.0, 202);
    assert_eq!(a_tools(&d, &scout).await, json!(["team", "memory"]));
    assert_eq!(connectors().await, json!([db]));
    assert!(said("approved the new tools for scout").await);
    assert_eq!(decide(&d, &ada, question_id, "approve").await.0, 409);

    // Auto mode applies it without a card and leaves a notice.
    auto(true).await;
    let outcome = d
        .state
        .set_agent_tools(ada_id, req(scout_id, Some(vec![CreateAgents, Team]), None))
        .await
        .unwrap();
    assert_eq!(outcome, AgentToolsOutcome::Applied);
    assert_eq!(a_tools(&d, &scout).await, json!(["team", "create_agents"]));
    let evs = events(&d, &ada).await;
    let notice = evs
        .iter()
        .find(|e| e["kind"] == "agent_tools_changed")
        .unwrap();
    assert_eq!(
        notice["payload"],
        json!({"name": "scout", "id": scout, "by": "ada", "auto": true})
    );
    assert_eq!(evs.iter().filter(|e| e["kind"] == "question").count(), 2);
}

#[tokio::test]
async fn agents_name_new_agents_by_the_operators_convention() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    create(&d, json!({"name": "ada", "instructions": "Be brief."})).await;
    create(&d, json!({"name": "bob", "tools": ["team"]})).await;
    let mut ada = connect_runner(&d.agents, &backend.token("ada")).await;

    let set = |names: Value| {
        let ui = d.ui.clone();
        async move {
            http(
                &ui,
                "PUT",
                "/v1/settings",
                Some(json!({"auto_mode": false, "agent_names": names})),
            )
            .await
        }
    };
    let (status, _) = set(json!("x".repeat(301))).await;
    assert_eq!(status, 400, "too long");
    let (status, saved) = set(json!("  Greek gods, lowercase  ")).await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["agent_names"], "Greek gods, lowercase");
    let DaemonFrame::Configure { instructions, .. } = next_frame(&mut ada).await else {
        panic!("expected new instructions");
    };
    assert!(instructions.starts_with("Be brief.\n\n"), "{instructions}");
    assert!(
        instructions.ends_with("Greek gods, lowercase"),
        "{instructions}"
    );

    // Who may not create agents does not get it; a runner connecting later does.
    let mut bob = open_runner(
        &d.agents,
        &backend.token("bob"),
        ferrobot_core::PROTOCOL_VERSION,
        false,
    )
    .await;
    let DaemonFrame::Welcome { instructions, .. } = next_frame(&mut bob).await else {
        panic!("expected a welcome");
    };
    assert_eq!(instructions, "", "bob cannot create agents");
    drop(ada);
    let mut ada = open_runner(
        &d.agents,
        &backend.token("ada"),
        ferrobot_core::PROTOCOL_VERSION,
        false,
    )
    .await;
    let DaemonFrame::Welcome { instructions, .. } = next_frame(&mut ada).await else {
        panic!("expected a welcome");
    };
    assert!(instructions.contains("Greek gods"), "{instructions}");

    let (status, cleared) = set(json!("   ")).await;
    assert_eq!((status, &cleared["agent_names"]), (200, &Value::Null));
}

/// Stands in for the host's `claude`: logs what it got and answers like `claude -p` stream-json.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
here="$(dirname "$0")"
case "$1" in
  --version) echo "9.9.9 (Claude Code)"; exit 0 ;;
  auth) echo '{"loggedIn": true, "authMethod": "claude.ai", "email": "someone@example.com"}'; exit 0 ;;
esac
case "$*" in *" /compact")
  echo "$*" >> "$here/compact-args.log"
  session="$(printf '%s\n' "$@" | sed -n 's/^--resume=//p')"
  printf '%s\n' "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Compacted\",\"session_id\":\"$session\"}"
  exit 0 ;;
esac
case "$*" in *" /usage")
  echo "$*" >> "$here/usage-args.log"
  env > "$here/usage-env.log"
  printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0,"result":"Signed in as someone@example.com\n\nCurrent session: 18% used · resets Nov 23 at 9:39pm (Europe/Berlin)\nCurrent week (all models): 63% used · resets Nov 26 at 6:59am (Europe/Berlin)\nCurrent week (Fable): 3% used"}'
  exit 0 ;;
esac
echo "$*" >> "$here/args.log"
env > "$here/env.log"
for a in "$@"; do case "$a" in --mcp-config=*) cat "${a#--mcp-config=}" > "$here/mcp.log" ;; esac; done
cat > "$here/prompt.log"
echo '{"type":"system","subtype":"init","session_id":"s1","tools":[]}'
echo '{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-5","content":[{"type":"text","text":"hello"}],"usage":{"input_tokens":1200,"cache_read_input_tokens":3000,"cache_creation_input_tokens":800,"output_tokens":50}},"parent_tool_use_id":null,"session_id":"s1"}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"hello","session_id":"s1","modelUsage":{"claude-sonnet-4-5":{"contextWindow":200000}}}'
"#;

async fn claude_code_turn(
    agents: &str,
    token: &str,
    prompt: &str,
    new_session: bool,
) -> (u16, String) {
    let bearer = format!("Bearer {token}");
    let body = json!({"prompt": prompt, "system": "be brief", "new_session": new_session});
    let headers = [
        ("authorization", bearer.as_str()),
        ("content-type", "application/json"),
    ];
    let path = "/host/claude-code/turn";
    let (status, _, out) =
        http_raw(agents, "POST", path, &headers, body.to_string().as_bytes()).await;
    (status, String::from_utf8_lossy(&out).into_owned())
}

#[cfg(unix)]
#[tokio::test]
async fn claude_code_agents_use_the_hosts_own_cli_and_nothing_enters_the_sandbox() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("claude");
    std::fs::write(&bin, FAKE_CLAUDE).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let backend = Arc::new(StatefulBackend::default());
    let data = dir.path().join("data");
    let data_dir = data.clone();
    let d = start_with(backend.clone(), None, "127.0.0.1:0", |s| {
        s.driver = "claude".into();
        s.claude_bin = bin.display().to_string();
        s.data_dir = data_dir;
    })
    .await;

    let (status, cc) = http(&d.ui, "GET", "/v1/claude-code", None).await;
    assert_eq!(status, 200, "{cc}");
    assert_eq!(
        (cc["installed"].as_bool(), cc["logged_in"].as_bool()),
        (Some(true), Some(true))
    );
    assert!(
        !cc.to_string().contains("example.com"),
        "never the account: {cc}"
    );

    let (_, provider) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(json!({"name": "cc", "kind": "claude_code", "default_model": "sonnet"})),
    )
    .await;
    let clio = create(&d, json!({"name": "clio", "provider_id": provider["id"]})).await;
    let context = format!("/v1/agents/{}/context", clio["id"].as_str().unwrap());
    let (_, ctx) = http(&d.ui, "GET", &context, None).await;
    assert_eq!(
        (&ctx["tracked"], &ctx["tokens"], &ctx["model"]),
        (&json!(true), &Value::Null, &json!("sonnet")),
        "no turn yet: {ctx}"
    );
    let compact = format!("/v1/agents/{}/compact", clio["id"].as_str().unwrap());
    assert_eq!(
        http(&d.ui, "POST", &compact, None).await.0,
        409,
        "nothing to compact"
    );
    let env = backend.envs.lock().unwrap()["clio"].clone();
    assert_eq!(env["FERROBOT_DRIVER"], "claude_code");
    // No key, no Claude login and no claude config: only ferrobot's own settings.
    assert!(env.keys().all(|k| k.starts_with("FERROBOT_")), "{env:?}");

    let token = env["FERROBOT_AGENT_TOKEN"].clone();
    let (status, out) = claude_code_turn(&d.agents, &token, "hi there", true).await;
    assert_eq!(status, 200, "{out}");
    let (_, ctx) = http(&d.ui, "GET", &context, None).await;
    assert_eq!(
        (&ctx["tokens"], &ctx["window"], &ctx["model"]),
        (&json!(5000), &json!(200_000), &json!("claude-sonnet-4-5")),
        "from claude's own stream: {ctx}"
    );
    // Its model call is a usage row, priced like the API would charge.
    let (_, usage) = http(&d.ui, "GET", "/v1/usage?group_by=model", None).await;
    let text = usage.to_string();
    assert!(
        text.contains("claude-sonnet-4-5"),
        "a claude_code row: {text}"
    );
    assert!(
        out.lines().any(|l| l.contains(r#""type":"result""#)),
        "{out}"
    );
    let log = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap_or_default();
    assert_eq!(
        log("prompt.log"),
        "hi there",
        "the prompt goes on stdin, not argv"
    );
    let first = log("args.log");
    assert!(
        first.contains("--tools= ") && first.contains("--strict-mcp-config"),
        "{first}"
    );
    assert!(first.contains("--model=sonnet"), "{first}");
    let session = first
        .split("--session-id=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    assert!(
        log("mcp.log").contains(&format!("Bearer {token}")),
        "the agent's own token for /mcp"
    );
    // Compacting runs claude's own /compact on that session, with no tools, on the daemon's word.
    assert_eq!(http(&d.ui, "POST", &compact, None).await.0, 204);
    let compacted = log("compact-args.log");
    assert!(
        compacted.contains(&format!("--resume={session}"))
            && compacted.contains("--tools= ")
            && compacted.trim_end().ends_with("/compact"),
        "{compacted}"
    );
    let (_, ctx) = http(&d.ui, "GET", &context, None).await;
    assert_eq!(
        ctx["tokens"],
        Value::Null,
        "the old size is gone until the next turn"
    );
    let kinds: Vec<_> = events(&d, clio["id"].as_str().unwrap())
        .await
        .into_iter()
        .map(|e| e["kind"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(kinds.iter().any(|k| k == "compacted"), "{kinds:?}");
    assert!(
        !log("env.log").contains(&token),
        "the token stays out of claude's environment"
    );

    let (status, _) = claude_code_turn(&d.agents, &token, "hi there", false).await;
    assert_eq!(status, 200);
    assert!(
        log("args.log")
            .lines()
            .nth(1)
            .unwrap()
            .contains(&format!("--resume={session}"))
    );

    // `claude` would run a leading slash command on the host.
    let (status, _) = claude_code_turn(&d.agents, &token, "/cost", false).await;
    assert_eq!(status, 400);

    // Agents on other providers cannot reach the host's claude.
    let (_, ollama) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(json!({"name": "ol", "kind": "ollama", "default_model": "qwen3"})),
    )
    .await;
    create(&d, json!({"name": "otto", "provider_id": ollama["id"]})).await;
    let other = backend.envs.lock().unwrap()["otto"]["FERROBOT_AGENT_TOKEN"].clone();
    let (status, _) = claude_code_turn(&d.agents, &other, "hi there", true).await;
    assert_eq!(status, 403);

    // Plan limits come from claude's own /usage: no model call, only the two limit lines.
    let (status, limits) = http(&d.ui, "GET", "/v1/limits", None).await;
    assert_eq!(status, 200, "{limits}");
    assert_eq!(
        limits.as_array().unwrap().len(),
        1,
        "not the Ollama provider: {limits}"
    );
    assert_eq!(limits[0]["kind"], "claude_code");
    assert_eq!(limits[0]["five_hour"]["used_percent"], 18.0);
    assert_eq!(
        limits[0]["weekly"]["resets"],
        "Nov 26 at 6:59am (Europe/Berlin)"
    );
    assert!(!limits.to_string().contains("example.com"), "{limits}");
    let usage = log("usage-args.log");
    for flag in [
        "-p",
        "--tools=",
        "--setting-sources=",
        "--no-session-persistence",
    ] {
        assert!(
            usage.split(' ').any(|a| a == flag),
            "{flag} missing: {usage}"
        );
    }
    assert!(
        !log("usage-env.log")
            .lines()
            .any(|l| l.starts_with("ANTHROPIC_"))
    );
    http(&d.ui, "GET", "/v1/limits", None).await;
    assert_eq!(
        log("usage-args.log").lines().count(),
        1,
        "read once a minute"
    );

    let id = clio["id"].as_str().unwrap();
    assert!(data.join("claude-code").join(id).join("session").exists());
    let (status, _) = http(&d.ui, "DELETE", &format!("/v1/agents/{id}"), None).await;
    assert_eq!(status, 204);
    assert!(
        !data.join("claude-code").join(id).exists(),
        "its folder goes with the agent"
    );
}

async fn tool_check(agents: &str, token: &str, command: &str) -> Value {
    let bearer = format!("Bearer {token}");
    let headers = [
        ("authorization", bearer.as_str()),
        ("content-type", "application/json"),
    ];
    let body = json!({ "command": command }).to_string();
    let (status, _, out) = http_raw(
        agents,
        "POST",
        "/runner/tool-check",
        &headers,
        body.as_bytes(),
    )
    .await;
    assert_eq!(status, 200);
    serde_json::from_slice(&out).unwrap()
}

#[tokio::test]
async fn risky_tool_calls_wait_for_a_card_unless_auto_mode_is_on() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let (_, ollama) = http(
        &d.ui,
        "POST",
        "/v1/providers",
        Some(json!({"name": "ol", "kind": "ollama", "default_model": "qwen3"})),
    )
    .await;
    let agent = create(&d, json!({"name": "toolie", "provider_id": ollama["id"]})).await;
    let id = agent["id"].as_str().unwrap().to_owned();
    let token = backend.envs.lock().unwrap()["toolie"]["FERROBOT_AGENT_TOKEN"].clone();

    assert_eq!(
        tool_check(&d.agents, &token, "cargo test").await["allow"],
        true
    );
    let blocked = tool_check(&d.agents, &token, "rm -rf /").await;
    assert_eq!(blocked["allow"], false);
    assert!(blocked["reason"].as_str().unwrap().starts_with("blocked"));

    // Risky: the call waits for the operator's card.
    let mut last = 0;
    for (answer, allowed) in [("approve", true), ("decline", false)] {
        let (agents, tok) = (d.agents.clone(), token.clone());
        let waiting =
            tokio::spawn(async move { tool_check(&agents, &tok, "git push origin main").await });
        let qid = approval_card(&d, &id, last).await;
        last = qid;
        assert_eq!(decide(&d, &id, qid, answer).await.0, 202);
        let got = waiting.await.unwrap();
        assert_eq!(got["allow"], allowed, "{answer}: {got}");
    }

    // Auto mode: risky calls run without a card; dangerous ones stay blocked.
    let auto = json!({"auto_mode": true});
    assert_eq!(http(&d.ui, "PUT", "/v1/settings", Some(auto)).await.0, 200);
    let before = events(&d, &id).await.len();
    assert_eq!(
        tool_check(&d.agents, &token, "git push origin main").await["allow"],
        true
    );
    assert_eq!(events(&d, &id).await.len(), before, "no card in Auto mode");
    assert_eq!(
        tool_check(&d.agents, &token, "mkfs.ext4 /dev/sda").await["allow"],
        false
    );
}

#[tokio::test]
async fn agent_tags_are_checked_and_kept() {
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let a = create(&d, json!({"name": "tia", "tags": [" ops ", "nightly"]})).await;
    assert_eq!(a["tags"], json!(["ops", "nightly"]));
    let path = format!("/v1/agents/{}", a["id"].as_str().unwrap());
    let too_long = "x".repeat(21);
    for bad in [
        json!(["a", "b", "c", "d", "e", "f"]),
        json!([" "]),
        json!([too_long]),
        json!(["ops", "ops "]),
    ] {
        let (status, _) = http(&d.ui, "PATCH", &path, Some(json!({ "tags": bad }))).await;
        assert_eq!(status, 400, "{bad}");
    }
    let (status, _) = http(
        &d.ui,
        "POST",
        "/v1/agents",
        Some(json!({"name": "tom", "tags": ["x", "x"]})),
    )
    .await;
    assert_eq!(status, 400);
    let (_, all) = http(&d.ui, "GET", "/v1/agents", None).await;
    assert_eq!(
        all.as_array().unwrap().len(),
        1,
        "a bad tag creates nothing"
    );

    let twenty = "é".repeat(20);
    let (_, a) = http(&d.ui, "PATCH", &path, Some(json!({ "tags": [twenty] }))).await;
    assert_eq!(a["tags"], json!([twenty]), "characters, not bytes");
    let (_, a) = http(&d.ui, "PATCH", &path, Some(json!({"description": "x"}))).await;
    assert_eq!(a["tags"], json!([twenty]), "omitted tags stay");
    let (_, a) = http(&d.ui, "PATCH", &path, Some(json!({"tags": []}))).await;
    assert_eq!(a["tags"], json!([]));
}

#[tokio::test]
async fn context_is_the_newest_call_since_the_last_new_chat() {
    use ferrobot_store::{Tokens, UsageRecord, UsageStatus};
    let d = start(Arc::new(StatefulBackend::default()), None, "127.0.0.1:0").await;
    let a = create(&d, json!({"name": "cora"})).await;
    let id: AgentId = a["id"].as_str().unwrap().parse().unwrap();
    let path = format!("/v1/agents/{id}/context");
    let (status, ctx) = http(&d.ui, "GET", &path, None).await;
    assert_eq!(status, 200, "{ctx}");
    assert_eq!(
        (&ctx["tracked"], &ctx["tokens"]),
        (&json!(true), &Value::Null)
    );

    let now = time::OffsetDateTime::now_utc();
    let row = |ago: i64, input: Option<i64>, status| UsageRecord {
        at: now - time::Duration::seconds(ago),
        agent_id: id,
        agent_name: "cora".into(),
        provider_id: None,
        provider_kind: "anthropic".into(),
        model: "claude-opus-5-5".into(),
        tokens: Tokens {
            input,
            output: Some(9),
            cache_read: Some(1000),
            cache_write: Some(500),
        },
        cost_usd: None,
        latency_ms: 5,
        status,
        turn_id: None,
    };
    for r in [
        row(60, Some(2000), UsageStatus::Ok),
        row(50, Some(1), UsageStatus::Error),
        row(40, None, UsageStatus::Ok),
    ] {
        d.state.store.insert_usage(r).await.unwrap();
    }
    let (_, ctx) = http(&d.ui, "GET", &path, None).await;
    assert_eq!(
        (&ctx["tokens"], &ctx["window"], &ctx["model"]),
        (&json!(3500), &json!(1_000_000), &json!("claude-opus-5-5")),
        "input plus cache of the newest call with counts"
    );

    let (status, _) = http(&d.ui, "POST", &format!("/v1/agents/{id}/new-session"), None).await;
    assert_eq!(status, 204);
    let (_, ctx) = http(&d.ui, "GET", &path, None).await;
    assert_eq!(ctx["tokens"], Value::Null, "a new chat starts empty");
    d.state
        .store
        .insert_usage(row(-60, Some(10), UsageStatus::Ok))
        .await
        .unwrap();
    let (_, ctx) = http(&d.ui, "GET", &path, None).await;
    assert_eq!(ctx["tokens"], 1510);
}

#[tokio::test]
async fn groups_talk_by_room_rules_and_a_call_to_one_lands_in_it() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let mut ids = Vec::new();
    for name in ["ann", "bob", "cy"] {
        let agent = create(&d, json!({ "name": name })).await;
        ids.push(agent["id"].as_str().unwrap().parse::<AgentId>().unwrap());
    }
    let [ann, bob, cy] = ids[..] else {
        unreachable!()
    };
    let unknown = "00000000-0000-4000-8000-000000000099";
    for (body, status) in [
        (json!({"name": " ", "members": [ann, bob]}), 400),
        (json!({"name": "Ops", "members": [ann, ann]}), 400),
        (json!({"name": "Ops", "members": [ann, unknown]}), 404),
    ] {
        let (got, err) = http(&d.ui, "POST", "/v1/groups", Some(body)).await;
        assert_eq!(got, status, "{err}");
    }
    let new = json!({"name": " Ops ", "members": [ann, bob]});
    let (status, group) = http(&d.ui, "POST", "/v1/groups", Some(new)).await;
    assert_eq!(status, 201, "{group}");
    assert_eq!(
        (&group["name"], &group["members"]),
        (&json!("Ops"), &json!([ann, bob]))
    );
    let id = group["id"].as_i64().unwrap();
    let path = format!("/v1/groups/{id}");
    assert_eq!(
        http(&d.ui, "GET", "/v1/groups", None).await.1,
        json!([group])
    );
    assert_eq!(http(&d.ui, "GET", "/v1/rooms", None).await.1, json!([]));
    let post = |body: &'static str| {
        let (ui, path) = (d.ui.clone(), format!("{path}/messages"));
        async move {
            let (status, sent) = http(&ui, "POST", &path, Some(json!({ "body": body }))).await;
            assert_eq!(status, 202, "{sent}");
            sent
        }
    };

    // Written once: members answer one at a time with the group note, out of their chats.
    let sent = post("Status?").await;
    assert_eq!(
        (&sent[0]["recipient"], &sent[0]["room_id"]),
        (&json!(ann), &json!(id))
    );
    let (_, room) = http(&d.ui, "GET", &format!("/v1/rooms/{id}"), None).await;
    assert_eq!(room["speaking"], json!(ann), "whose turn it is");
    let mut ann_ws = connect_runner(&d.agents, &backend.token("ann")).await;
    let mut bob_ws = connect_runner(&d.agents, &backend.token("bob")).await;
    let body = say_turn(&mut ann_ws, "All green.").await;
    assert!(body.starts_with("(Group chat:"), "{body}");
    assert!(
        !body.contains("Voice call") && !body.contains("You host"),
        "{body}"
    );
    assert!(
        body.ends_with("In the group since your last turn:\nThe operator: Status?"),
        "{body}"
    );
    let body = say_turn(&mut bob_ws, "Agreed.").await;
    assert!(
        body.ends_with("The operator: Status?\n@ann: All green."),
        "{body}"
    );
    assert!(
        say_turn(&mut ann_ws, "(pass)")
            .await
            .ends_with("@bob: Agreed.")
    );
    turns_done(&d, ann, 2).await;
    quiet(&mut bob_ws).await;
    let lines = lines_reach(&d, id, 3).await;
    let said: Vec<_> = lines.iter().map(|l| l.1.as_str()).collect();
    assert_eq!(said, ["Status?", "All green.", "Agreed."]);
    assert!(received(&d, ann).await.is_empty() && received(&d, bob).await.is_empty());

    let change = json!({"name": "Ops team", "members": [cy, bob]});
    let (status, changed) = http(&d.ui, "PATCH", &path, Some(change)).await;
    assert_eq!(status, 200, "{changed}");
    assert_eq!(
        (&changed["name"], &changed["members"]),
        (&json!("Ops team"), &json!([bob, cy])),
        "bob keeps its place"
    );

    // Called, the group is the call's room: members are fixed and nothing is written meanwhile.
    let (status, view) = http(&d.ui, "POST", &format!("{path}/call"), None).await;
    assert_eq!(
        (status, view),
        (200, json!({"agents": [bob, cy], "room_id": id}))
    );
    assert_eq!(
        http(&d.ui, "POST", &format!("{path}/call"), None).await.0,
        409
    );
    let change = json!({"name": "Renamed", "members": [ann, bob]});
    assert_eq!(http(&d.ui, "PATCH", &path, Some(change)).await.0, 409);
    assert_eq!(http(&d.ui, "DELETE", &path, None).await.0, 409);
    let (_, room) = http(&d.ui, "GET", &format!("/v1/rooms/{id}"), None).await;
    assert_eq!(room["name"], "Ops team");
    post("Next?").await;
    let body = say_turn(&mut bob_ws, "Deploying.").await;
    assert!(
        body.starts_with("(Voice call:") && body.contains("(You host the call."),
        "{body}"
    );
    let mut cy_ws = connect_runner(&d.agents, &backend.token("cy")).await;
    assert!(
        say_turn(&mut cy_ws, "(pass)")
            .await
            .ends_with("@bob: Deploying.")
    );
    lines_reach(&d, id, 5).await;
    // Hanging up cuts off a turn still holding the floor, so cy's pass must be settled first.
    turns_done(&d, cy, 1).await;

    // Hung up, the group stays and talks outside the call again.
    let hang_up = json!({"agents": []});
    assert_eq!(http(&d.ui, "PUT", "/v1/call", Some(hang_up)).await.0, 200);
    let (_, room) = http(&d.ui, "GET", &format!("/v1/rooms/{id}"), None).await;
    assert_eq!(room["ended_at"], Value::Null);
    assert_eq!(http(&d.ui, "GET", "/v1/rooms", None).await.1, json!([]));
    post("Thanks").await;
    assert!(
        say_turn(&mut bob_ws, "(pass)")
            .await
            .starts_with("(Group chat:")
    );
    assert!(
        say_turn(&mut cy_ws, "(pass)")
            .await
            .starts_with("(Group chat:")
    );
    turns_done(&d, cy, 2).await;

    // A line naming a member is that member's to answer; the others only hear it.
    let sent = post("@cy ship it?").await;
    assert_eq!(sent[0]["recipient"], json!(cy));
    say_turn(&mut cy_ws, "Shipped.").await;
    turns_done(&d, cy, 3).await;
    quiet(&mut bob_ws).await;
    let (_, room) = http(&d.ui, "GET", &format!("/v1/rooms/{id}"), None).await;
    assert_eq!(room["speaking"], Value::Null, "nobody's turn");

    assert_eq!(http(&d.ui, "DELETE", &path, None).await.0, 204);
    assert_eq!(
        http(&d.ui, "GET", &format!("/v1/rooms/{id}"), None).await.0,
        404
    );
    assert_eq!(http(&d.ui, "GET", "/v1/groups", None).await.1, json!([]));
    assert_eq!(room_mail(&d, cy).await, 0, "its turns went with it");
}

#[tokio::test]
async fn operator_uploads_land_in_the_agents_workspace() {
    let backend = Arc::new(StatefulBackend::default());
    let d = start(backend.clone(), None, "127.0.0.1:0").await;
    let ann = create(&d, json!({"name": "ann"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let upload = async |agent: &str, name: &str, auth: &[(&str, &str)], body: &[u8]| {
        let path = format!("/v1/agents/{agent}/uploads?name={name}");
        let (status, _, body) = http_raw(&d.ui, "POST", &path, auth, body).await;
        (
            status,
            serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null),
        )
    };

    let (status, body) = upload(&ann, "..%2Fsecret%2FReport%20(1).pdf", &[], &[7; 1234]).await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(
        body,
        json!({"path": "uploads/Report__1_.pdf", "size": 1234})
    );
    assert_eq!(
        *backend.written.lock().unwrap(),
        [(
            "/home/node/workspace/uploads/Report__1_.pdf".to_owned(),
            1234
        )]
    );

    let big = vec![0; 25 * 1024 * 1024 + 1];
    let (status, body) = upload(&ann, "big.bin", &[], &big).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        upload(&ann, "x.txt", &[("Authorization", "")], b"hi")
            .await
            .0,
        401
    );
    let unknown = "00000000-0000-0000-0000-000000000001";
    assert_eq!(upload(unknown, "x.txt", &[], b"hi").await.0, 404);
    assert_eq!(
        backend.written.lock().unwrap().len(),
        1,
        "nothing else written"
    );
}
