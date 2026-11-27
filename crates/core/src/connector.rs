//! Connectors: remote MCP servers whose tools the daemon offers to the agents
//! they are assigned to. The token lives in the daemon's secret store.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// A remote MCP server whose tools assigned agents can use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct Connector {
    pub id: Uuid,
    /// 1-16 chars, `a-z0-9-`, starts with a letter. Prefixes its tools
    /// (`<name>__<tool>`).
    pub name: String,
    /// Streamable HTTP endpoint of a remote MCP server; empty for a local one.
    pub url: String,
    /// A local MCP server: its program and arguments, started on the daemon's computer without
    /// a shell (e.g. `["npx", "-y", "@scope/server"]`). `None`: a remote server at `url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// The environment variable a local server gets the connector's Vault token in, e.g.
    /// `GITHUB_TOKEN`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
    /// The server's tools agents may use, by the server's own names; `None`: all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

/// How a connector an agent proposes signs in (`add_connector` tool).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ConnectorAuth {
    /// A Bearer token the operator types into the approval card.
    Token,
    /// An OAuth sign-in the operator does in Settings after adding it.
    Oauth,
    /// No sign-in.
    None,
}

/// Longest connector name; keeps `mcp__ferrobot__<name>__<tool>` short.
pub const MAX_CONNECTOR_NAME: usize = 16;
/// Most arguments a local server's command takes, and the longest one.
const MAX_ARGS: usize = 32;
/// Tools one connector may be limited to.
const MAX_TOOLS: usize = 500;
const MAX_ARG_BYTES: usize = 1024;

impl Connector {
    /// Returns a human-readable reason when a field is invalid.
    pub fn validate(&self) -> Result<(), String> {
        let n = self.name.as_bytes();
        let name_ok = !n.is_empty()
            && n.len() <= MAX_CONNECTOR_NAME
            && n[0].is_ascii_lowercase()
            && n.iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
        if !name_ok {
            return Err(format!(
                "name must be 1-{MAX_CONNECTOR_NAME} chars of a-z, 0-9 and -, starting with a letter"
            ));
        }
        if let Some(env) = &self.token_env {
            let e = env.as_bytes();
            let env_ok = !e.is_empty()
                && e.len() <= 64
                && (e[0].is_ascii_uppercase() || e[0] == b'_')
                && e.iter()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_');
            if !env_ok {
                return Err(
                    "token_env must be an environment variable name like GITHUB_TOKEN".into(),
                );
            }
        }
        let tool_ok = |t: &String| {
            !t.is_empty()
                && t.len() <= 128
                && t.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        };
        if self
            .tools
            .as_ref()
            .is_some_and(|t| t.len() > MAX_TOOLS || !t.iter().all(tool_ok))
        {
            return Err(format!(
                "tools are at most {MAX_TOOLS} tool names of letters, digits, _ and -"
            ));
        }
        if let Some(argv) = &self.command {
            let program_ok = argv.first().is_some_and(|p| {
                !p.is_empty() && !p.chars().any(|c| c.is_whitespace() || c.is_control())
            });
            if !program_ok
                || argv.len() > MAX_ARGS
                || argv
                    .iter()
                    .any(|a| a.len() > MAX_ARG_BYTES || a.contains('\0'))
            {
                return Err(format!(
                    "command must be a program and at most {MAX_ARGS} arguments of up to {MAX_ARG_BYTES} bytes"
                ));
            }
            if !self.url.is_empty() {
                return Err("a local server has a command, not a url".into());
            }
        } else {
            if self.token_env.is_some() {
                return Err("token_env is only for local servers".into());
            }
            if !(self.url.starts_with("https://") || self.url.starts_with("http://")) {
                return Err("url must be an http(s) URL".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_name_and_url() {
        let c = |name: &str, url: &str| Connector {
            id: Uuid::nil(),
            name: name.into(),
            url: url.into(),
            command: None,
            token_env: None,
            tools: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
        };
        assert!(
            c("github", "https://api.example.com/mcp")
                .validate()
                .is_ok()
        );
        assert!(c("my-db-2", "http://127.0.0.1:9000/mcp").validate().is_ok());
        for bad in ["", "GitHub", "2fa", "a_b", "seventeen-chars-x"] {
            assert!(c(bad, "https://x").validate().is_err(), "{bad}");
        }
        assert!(c("ok", "ftp://x").validate().is_err());
        let scoped = |tools: Vec<&str>| Connector {
            tools: Some(tools.into_iter().map(Into::into).collect()),
            ..c("gh", "https://x")
        };
        assert!(scoped(vec!["list_issues", "get-pr"]).validate().is_ok());
        assert!(scoped(vec!["bad name"]).validate().is_err());

        let local = |argv: &[&str], env: Option<&str>| Connector {
            command: Some(argv.iter().map(|a| (*a).to_owned()).collect()),
            token_env: env.map(Into::into),
            ..c("files", "")
        };
        assert!(
            local(&["npx", "-y", "@scope/server"], Some("GITHUB_TOKEN"))
                .validate()
                .is_ok()
        );
        for (argv, env) in [
            (&[][..], None),
            (&["npx -y x"][..], None),
            (&["npx"][..], Some("lower")),
            (&["npx", "a\0b"][..], None),
        ] {
            assert!(local(argv, env).validate().is_err(), "{argv:?} {env:?}");
        }
        assert!(
            Connector {
                url: "https://x".into(),
                ..local(&["npx"], None)
            }
            .validate()
            .is_err(),
            "not both"
        );
    }
}
