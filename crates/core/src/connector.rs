//! Connectors: remote MCP servers whose tools the daemon offers to the agents
//! they are assigned to. The token lives in the daemon's secret store.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct Connector {
    pub id: Uuid,
    /// 1-16 chars, `a-z0-9-`, starts with a letter. Prefixes its tools
    /// (`<name>__<tool>`).
    pub name: String,
    /// Streamable HTTP endpoint of the MCP server.
    pub url: String,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

/// Longest connector name; keeps `mcp__ferrobot__<name>__<tool>` short.
pub const MAX_CONNECTOR_NAME: usize = 16;

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
        if !(self.url.starts_with("https://") || self.url.starts_with("http://")) {
            return Err("url must be an http(s) URL".into());
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
    }
}
