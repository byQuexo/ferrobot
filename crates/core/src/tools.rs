//! Tool profiles: which groups of tools an agent may use. Connectors are not groups here; an
//! agent's connectors are its assignment (`/v1/agents/{id}/connectors`).

use serde::{Deserialize, Serialize};

/// A group of tools an agent can have on or off. The first six are ferrobot's own MCP tools
/// (enforced by the daemon), the last three the model's built-in tools (enforced by the runner).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ToolGroup {
    /// `list_agents`, `send_message`, `read_mailbox`, `assign_task`, `list_tasks`, `call_agents`.
    Team,
    /// `remember`, `recall`, `forget`.
    Memory,
    /// `ask_user`, `share_file`, `react`, `invite_to_call`, `hang_up`, `send_to_discord`.
    Operator,
    /// `create_agent`, `set_agent_tools`.
    CreateAgents,
    /// `add_connector`.
    AddConnectors,
    /// `create_routine`.
    Routines,
    /// The shell, and with it every CLI on the agent's computer.
    Shell,
    /// The model's own file tools (read, write, edit).
    Files,
    /// The model's own web tools (fetch, search).
    Web,
}

impl ToolGroup {
    pub const ALL: [Self; 9] = [
        Self::Team,
        Self::Memory,
        Self::Operator,
        Self::CreateAgents,
        Self::AddConnectors,
        Self::Routines,
        Self::Shell,
        Self::Files,
        Self::Web,
    ];

    /// Every group: the profile of an agent nobody narrowed.
    pub fn all() -> Vec<Self> {
        Self::ALL.to_vec()
    }

    /// Built into the model's harness, so the runner has to turn it off.
    pub fn is_builtin(self) -> bool {
        matches!(self, Self::Shell | Self::Files | Self::Web)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Team => "team",
            Self::Memory => "memory",
            Self::Operator => "operator",
            Self::CreateAgents => "create_agents",
            Self::AddConnectors => "add_connectors",
            Self::Routines => "routines",
            Self::Shell => "shell",
            Self::Files => "files",
            Self::Web => "web",
        }
    }

    /// Sorted and without duplicates, the form profiles are compared and stored in.
    pub fn normalize(mut groups: Vec<Self>) -> Vec<Self> {
        groups.sort_unstable();
        groups.dedup();
        groups
    }
}

impl std::fmt::Display for ToolGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_serde() {
        for g in ToolGroup::ALL {
            assert_eq!(serde_json::to_value(g).unwrap(), g.as_str());
        }
        assert_eq!(
            ToolGroup::normalize(ToolGroup::ALL.to_vec()),
            ToolGroup::all()
        );
    }
}
