use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::CoreError;

/// Stable, opaque identifier of an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "schema", schema(value_type = String, format = Uuid))]
#[serde(transparent)]
pub struct AgentId(Uuid);

impl AgentId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for AgentId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for AgentId {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s)
            .map(Self)
            .map_err(|_| CoreError::InvalidAgentId(s.to_owned()))
    }
}

/// Unique agent handle like `build-bot`: 1-32 chars of `a-z0-9-`, starting with a letter and
/// not ending with `-`, so it stays DNS-safe in sandbox resource names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(
    feature = "schema",
    schema(value_type = String, pattern = "^[a-z]([a-z0-9-]{0,30}[a-z0-9])?$", example = "alice")
)]
#[serde(try_from = "String", into = "String")]
pub struct AgentName(String);

impl AgentName {
    pub const MAX_LEN: usize = 32;

    pub fn parse(raw: impl Into<String>) -> Result<Self, CoreError> {
        let name = raw.into();
        let invalid = |reason| CoreError::InvalidAgentName {
            name: name.clone(),
            reason,
        };
        if name.is_empty() || name.len() > Self::MAX_LEN {
            return Err(invalid("must be 1-32 characters"));
        }
        if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
            return Err(invalid("must start with a lowercase letter"));
        }
        if name.ends_with('-') {
            return Err(invalid("must not end with '-'"));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(invalid("only a-z, 0-9 and '-' are allowed"));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AgentName {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<AgentName> for String {
    fn from(value: AgentName) -> Self {
        value.0
    }
}

impl fmt::Display for AgentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Lifecycle state of an agent's sandbox as tracked by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Registered, sandbox not created yet.
    Created,
    /// Sandbox running; the runner may or may not be connected.
    Running,
    /// Sandbox paused (warm sleep): memory kept, wakes without a restart.
    /// Messages queue up until it wakes.
    Paused,
    /// Sandbox stopped (disk kept). Messages queue up until it wakes.
    Stopped,
    /// Sandbox moved to object storage; waking restores it.
    Archived,
    /// Sandbox creation or start failed; see daemon logs.
    Failed,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
            Self::Archived => "archived",
            Self::Failed => "failed",
        }
    }
}

impl FromStr for AgentStatus {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "created" => Ok(Self::Created),
            "running" => Ok(Self::Running),
            "paused" => Ok(Self::Paused),
            "stopped" => Ok(Self::Stopped),
            "archived" => Ok(Self::Archived),
            "failed" => Ok(Self::Failed),
            other => Err(CoreError::UnknownVariant {
                kind: "agent status",
                value: other.to_owned(),
            }),
        }
    }
}

/// Body shape of the generated character avatar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CharacterBody {
    Blob,
    Bean,
    Pebble,
    Sprout,
}

/// Eye style of the generated character avatar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CharacterEyes {
    Dots,
    Ovals,
    Wide,
    Visor,
}

/// A chosen character avatar look; without one, clients derive a look from the agent id.
/// An uploaded avatar image wins over both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct AvatarCharacter {
    pub body: CharacterBody,
    pub eyes: CharacterEyes,
    /// Color hue in degrees, 0..=359.
    #[cfg_attr(feature = "schema", schema(minimum = 0, maximum = 359))]
    pub hue: u16,
}

impl AvatarCharacter {
    /// Whether the hue is in range.
    pub fn is_valid(&self) -> bool {
        self.hue <= 359
    }
}

/// A registered agent. Secrets (the runner token) are never part of this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct Agent {
    pub id: AgentId,
    /// Display and addressing name; unique, can be changed (rename).
    pub name: AgentName,
    /// Stable name of the agent's sandbox resources (container, disk). Set
    /// at creation and kept on rename.
    pub sandbox_name: String,
    /// One-line description shown in agent lists.
    pub description: String,
    /// Operator-provided role handed to the model as system instructions.
    pub instructions: String,
    /// Changes whenever the avatar changes; positive while an avatar exists.
    pub avatar_rev: i64,
    /// Chosen character avatar look; `null` means derived from the id.
    pub character: Option<AvatarCharacter>,
    /// Agent that created this one (with operator approval); `null` means
    /// the operator created it.
    pub created_by: Option<AgentId>,
    /// Model provider (`GET /v1/providers`); `null` uses the daemon's
    /// `ANTHROPIC_API_KEY`.
    pub provider_id: Option<uuid::Uuid>,
    /// Model id; `null` uses the provider's default model.
    pub model: Option<String>,
    pub status: AgentStatus,
    /// Backend kind that owns the sandbox, e.g. `docker`.
    pub backend: Option<String>,
    /// Backend-specific sandbox identifier (container id, sprite name, ...).
    pub sandbox_id: Option<String>,
    /// Last model session id reported by the runner, used for resume.
    pub session_id: Option<String>,
    /// Pinned to the top of the operator's agent list.
    #[serde(default)]
    pub pinned: bool,
    /// Tool groups the agent may use; its connectors are its assignment.
    #[serde(default = "crate::ToolGroup::all")]
    pub tools: Vec<crate::ToolGroup>,
    /// The operator's labels for the agent, in order.
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_names() {
        let too_long = "a".repeat(33);
        let valid = ["a", "alice", "build-bot", "x1", "a-b-c"].map(|n| (n, true));
        let invalid = [
            "",
            "Alice",
            "1abc",
            "-a",
            "a-",
            "a_b",
            "a b",
            "ä",
            too_long.as_str(),
        ];
        for (name, ok) in valid.into_iter().chain(invalid.map(|n| (n, false))) {
            assert_eq!(AgentName::parse(name).is_ok(), ok, "{name:?}");
        }
    }

    #[test]
    fn name_deserialization_validates() {
        let err = serde_json::from_str::<AgentName>("\"Bad Name\"");
        assert!(err.is_err());
        let ok: AgentName = serde_json::from_str("\"bob\"").unwrap();
        assert_eq!(ok.as_str(), "bob");
    }

    #[test]
    fn status_roundtrip() {
        for s in [
            AgentStatus::Created,
            AgentStatus::Running,
            AgentStatus::Stopped,
            AgentStatus::Failed,
        ] {
            assert_eq!(s.as_str().parse::<AgentStatus>().unwrap(), s);
        }
    }
}
