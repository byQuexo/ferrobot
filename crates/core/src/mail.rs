use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{AgentId, CoreError};

/// Who sent a mailbox message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Sender {
    /// The human operator, via the UI or HTTP API.
    User,
    /// Another agent, via the `send_message` MCP tool.
    Agent(AgentId),
    /// A scheduled routine (by routine id).
    Routine(i64),
}

impl fmt::Display for Sender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Agent(id) => write!(f, "agent:{id}"),
            Self::Routine(id) => write!(f, "routine:{id}"),
        }
    }
}

impl FromStr for Sender {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "user" {
            return Ok(Self::User);
        }
        if let Some(id) = s.strip_prefix("routine:") {
            return id
                .parse()
                .map(Self::Routine)
                .map_err(|_| CoreError::InvalidSender(s.to_owned()));
        }
        s.strip_prefix("agent:")
            .and_then(|id| id.parse().ok())
            .map(Self::Agent)
            .ok_or_else(|| CoreError::InvalidSender(s.to_owned()))
    }
}

/// Delivery state: `pending`, `in_flight`, `delivered`; back to `pending` when a runner
/// disconnects mid-turn, `failed` after too many attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MailStatus {
    Pending,
    InFlight,
    Delivered,
    Failed,
}

impl MailStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InFlight => "in_flight",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
        }
    }
}

impl FromStr for MailStatus {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "in_flight" => Ok(Self::InFlight),
            "delivered" => Ok(Self::Delivered),
            "failed" => Ok(Self::Failed),
            other => Err(CoreError::UnknownVariant {
                kind: "mail status",
                value: other.to_owned(),
            }),
        }
    }
}

/// One message in an agent's mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct MailMessage {
    pub id: i64,
    pub sender: Sender,
    pub recipient: AgentId,
    /// Text; mentions of agents are stored as `<@agent-id>` tokens (see
    /// `ferrobot_core::mention`), so render them with the current names.
    pub body: String,
    pub reply_to: Option<i64>,
    /// `true`: wakes the recipient and goes first; `false`: waits for its next turn
    /// without waking it; `null` (default): queues and wakes.
    pub priority: Option<bool>,
    /// The call room whose turn this message carries; such messages stay out of the agent's chat.
    pub room_id: Option<i64>,
    pub status: MailStatus,
    pub attempts: u32,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_roundtrip() {
        let id = AgentId::new();
        for s in [Sender::User, Sender::Agent(id), Sender::Routine(7)] {
            assert_eq!(s.to_string().parse::<Sender>().unwrap(), s);
        }
        assert!("agent:nope".parse::<Sender>().is_err());
        assert!("robot".parse::<Sender>().is_err());
    }

    #[test]
    fn status_roundtrip() {
        for s in [
            MailStatus::Pending,
            MailStatus::InFlight,
            MailStatus::Delivered,
            MailStatus::Failed,
        ] {
            assert_eq!(s.as_str().parse::<MailStatus>().unwrap(), s);
        }
    }
}
