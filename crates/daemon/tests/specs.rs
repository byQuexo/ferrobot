//! Keeps the committed API contracts in `protocol/` in sync with the Rust types.
//!
//! Regenerate after changing an API type:
//! `FERROBOT_UPDATE_SPECS=1 cargo test -p ferrobot-daemon --test specs`

use std::path::PathBuf;

use ferrobot_core::{Agent, AgentId, AgentName, AgentStatus, MailMessage, MailStatus, Sender};
use ferrobot_daemon::views::{AgentView, Presence};
use ferrobot_daemon::{UiEvent, spec};
use ferrobot_store::StoredEvent;
use serde_json::json;
use time::OffsetDateTime;

fn protocol_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol")
}

#[test]
fn committed_specs_are_up_to_date() {
    let update = std::env::var_os("FERROBOT_UPDATE_SPECS").is_some();
    let mut stale = Vec::new();
    for (name, generated) in spec::generated_files() {
        let path = protocol_dir().join(name);
        if update {
            std::fs::write(&path, &generated).unwrap();
            continue;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        if committed != generated {
            stale.push(name);
        }
    }
    assert!(
        stale.is_empty(),
        "stale API specs {stale:?}; run `FERROBOT_UPDATE_SPECS=1 cargo test -p ferrobot-daemon --test specs`"
    );
}

#[test]
fn every_routed_path_is_documented() {
    let doc = spec::openapi();
    let documented: Vec<&str> = doc.paths.paths.keys().map(String::as_str).collect();
    for path in [
        "/v1/health",
        "/v1/ws",
        "/v1/agents",
        "/v1/agents/{id}",
        "/v1/agents/{id}/avatar",
        "/v1/agents/{id}/messages",
        "/v1/agents/{id}/events",
        "/v1/agents/{id}/read",
        "/v1/conversations",
        "/v1/conversations/{a}/{b}",
        "/v1/agents/{id}/interrupt",
        "/v1/agents/{id}/memory",
        "/v1/agents/{id}/memory/{key}",
        "/v1/agents/{id}/routines",
        "/v1/routines/{rid}",
        "/v1/routines/{rid}/run",
        "/v1/routines/{rid}/runs",
        "/v1/agents/{id}/sandbox",
        "/v1/agents/{id}/stop",
        "/v1/agents/{id}/wake",
        "/v1/agents/{id}/exec",
        "/v1/agents/{id}/snapshots",
        "/v1/agents/{id}/snapshots/{sid}",
        "/v1/agents/{id}/snapshots/{sid}/restore",
    ] {
        assert!(documented.contains(&path), "{path} missing from OpenAPI");
    }
    assert_eq!(
        documented.len(),
        24,
        "undocumented extra paths: {documented:?}"
    );
}

fn validator(schema: &serde_json::Value) -> jsonschema::Validator {
    jsonschema::validator_for(schema).unwrap()
}

#[test]
fn shared_fixtures_validate_against_runner_schema() {
    let v = validator(&spec::runner_protocol_schema());
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol/fixtures");
    let mut checked = 0;
    for side in ["daemon", "runner"] {
        for entry in std::fs::read_dir(fixtures.join(side)).unwrap() {
            let path = entry.unwrap().path();
            let frame: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let errors: Vec<String> = v.iter_errors(&frame).map(|e| e.to_string()).collect();
            assert!(errors.is_empty(), "{}: {errors:?}", path.display());
            checked += 1;
        }
    }
    assert!(
        checked >= 9,
        "expected the shared fixtures, found {checked}"
    );
    let bogus = json!({"type": "turn", "turn_id": "t"});
    assert!(!v.is_valid(&bogus), "incomplete frame must not validate");
}

#[test]
fn ui_events_validate_against_schema() {
    let v = validator(&spec::ui_events_schema());
    let agent = Agent {
        id: AgentId::new(),
        name: AgentName::parse("alice").unwrap(),
        sandbox_name: "alice".into(),
        description: "Researcher".into(),
        instructions: String::new(),
        avatar_rev: 2,
        status: AgentStatus::Running,
        backend: Some("docker".into()),
        sandbox_id: Some("ferrobot-alice".into()),
        session_id: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
    };
    let message = MailMessage {
        priority: None,
        id: 1,
        sender: Sender::Agent(agent.id),
        recipient: AgentId::new(),
        body: "hi".into(),
        reply_to: None,
        status: MailStatus::Pending,
        attempts: 0,
        created_at: OffsetDateTime::UNIX_EPOCH,
    };
    let event = StoredEvent {
        id: 7,
        agent_id: agent.id,
        turn_id: Some("t1".into()),
        kind: "assistant_text".into(),
        payload: json!({"kind": "assistant_text", "text": "hello"}),
        created_at: OffsetDateTime::UNIX_EPOCH,
    };
    for ev in [
        UiEvent::Agent {
            agent: AgentView {
                agent: agent.clone(),
                connected: true,
                presence: Presence::Working,
                unread: 3,
                avatar_url: Some(format!("/v1/agents/{}/avatar?v=2", agent.id)),
            },
        },
        UiEvent::Delta {
            agent_id: agent.id,
            turn_id: "t1".into(),
            text: "hel".into(),
        },
        UiEvent::MemoryChanged { agent_id: agent.id },
        UiEvent::RoutinesChanged { agent_id: agent.id },
        UiEvent::AgentDeleted { agent_id: agent.id },
        UiEvent::Message { message },
        UiEvent::Event { event },
    ] {
        let value = serde_json::to_value(&ev).unwrap();
        let errors: Vec<String> = v.iter_errors(&value).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{value}: {errors:?}");
    }
    assert!(!v.is_valid(&json!({"type": "agent_deleted", "agent_id": 5})));
}
