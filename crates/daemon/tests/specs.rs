//! Keeps the committed API contracts in `protocol/` in sync with the Rust types.
//!
//! Regenerate after changing an API type:
//! `FERROBOT_UPDATE_SPECS=1 cargo test -p ferrobot-daemon --test specs`

use std::path::PathBuf;

use ferrobot_core::{Agent, AgentId, AgentName, AgentStatus, MailMessage, MailStatus, Sender};
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
        "/v1/agents",
        "/v1/agents/{id}",
        "/v1/agents/{id}/sandbox",
        "/v1/agents/{id}/messages",
        "/v1/agents/{id}/events",
        "/v1/agents/{id}/interrupt",
        "/v1/agents/{id}/stop",
        "/v1/agents/{id}/wake",
        "/v1/agents/{id}/snapshot",
        "/v1/agents/{id}/exec",
        "/v1/ws",
    ] {
        assert!(documented.contains(&path), "{path} missing from OpenAPI");
    }
    assert_eq!(
        documented.len(),
        12,
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
        instructions: String::new(),
        status: AgentStatus::Running,
        backend: Some("docker".into()),
        sandbox_id: Some("ferrobot-alice".into()),
        session_id: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
    };
    let message = MailMessage {
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
            agent: agent.clone(),
            connected: true,
        },
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
