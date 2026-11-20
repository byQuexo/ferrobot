//! JSON frames on the runner's WebSocket, dialled out by the runner so sleeping sandboxes just
//! reconnect. Mirrored by `runner/src/protocol.ts`; both sides parse `protocol/fixtures/`.

use serde::{Deserialize, Serialize};

use crate::{AgentId, AgentName, Sender};

/// Frames sent from the daemon to a runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonFrame {
    /// First frame after a successful `hello`.
    Welcome {
        protocol_version: u32,
        agent_id: AgentId,
        agent_name: AgentName,
        instructions: String,
        /// Session to resume, if the daemon knows one.
        resume_session_id: Option<String>,
    },
    /// Run one turn for a mailbox message.
    Turn {
        turn_id: String,
        /// `0`: a new agent's greeting turn, which has no mailbox message.
        message_id: i64,
        sender: Sender,
        /// Display name of the sender (`user` or the agent name).
        sender_name: String,
        body: String,
    },
    /// A message that arrived during turn `turn_id` (only with `hello.steering`); queued again
    /// unless `turn_done.steered` lists it.
    Steer {
        turn_id: String,
        message_id: i64,
        sender: Sender,
        /// Display name of the sender (`user` or the agent name).
        sender_name: String,
        body: String,
    },
    /// Abort the running turn, if it matches.
    Interrupt { turn_id: String },
    /// The operator changed the agent's name, instructions or connectors;
    /// applies from the next turn.
    Configure {
        agent_name: AgentName,
        instructions: String,
    },
}

/// Frames sent from a runner to the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunnerFrame {
    /// First frame on every connection.
    Hello {
        protocol_version: u32,
        runner_version: String,
        /// `claude`, `openai`, `codex`, `acp` or `mock`.
        driver: String,
        /// Session id persisted on the sandbox disk, if any.
        session_id: Option<String>,
        /// The driver can take messages during a turn (`steer`).
        #[serde(default)]
        steering: bool,
    },
    /// Progress inside a turn.
    Event { turn_id: String, event: TurnEvent },
    /// Turn finished (successfully or not). Exactly one per `Turn`.
    TurnDone {
        turn_id: String,
        outcome: TurnOutcome,
        session_id: Option<String>,
        /// Final assistant text, if any.
        result: Option<String>,
        error: Option<String>,
        /// Ids of `steer` messages the model saw during this turn.
        #[serde(default)]
        steered: Vec<i64>,
    },
}

/// Observable steps of a turn, persisted and streamed to the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnEvent {
    /// Final text of one assistant message.
    AssistantText {
        text: String,
    },
    /// Streaming fragment of the assistant message being written. Forwarded
    /// live to UI clients but not persisted; `assistant_text` follows.
    AssistantDelta {
        text: String,
    },
    ToolUse {
        /// Model-assigned id that links the call to its `tool_result`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        #[cfg_attr(feature = "schema", schema(value_type = Object))]
        input: serde_json::Value,
    },
    ToolResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Tool output as text, truncated by the runner.
        output: String,
        is_error: bool,
    },
    ToolDenied {
        name: String,
        reason: String,
    },
    Log {
        level: String,
        message: String,
    },
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_done_shape_is_stable() {
        let frame = RunnerFrame::TurnDone {
            turn_id: "t1".into(),
            outcome: TurnOutcome::Completed,
            session_id: Some("s1".into()),
            result: Some("ok".into()),
            error: None,
            steered: vec![],
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], "turn_done");
        assert_eq!(json["outcome"], "completed");
    }

    #[test]
    fn event_shape_is_stable() {
        let frame = RunnerFrame::Event {
            turn_id: "t1".into(),
            event: TurnEvent::ToolUse {
                id: None,
                name: "mcp__ferrobot__send_message".into(),
                input: serde_json::json!({"to": "bob"}),
            },
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["event"]["kind"], "tool_use");
    }
}
