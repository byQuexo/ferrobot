//! Model providers: where an agent's model requests go and how they are
//! authenticated. Credentials themselves never appear in these types.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::CoreError;

/// The API family of a provider. Decides the runner driver, the upstream
/// URL and how the daemon's model proxy adds the credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Anthropic API (`x-api-key`).
    Anthropic,
    /// `OpenAI` API.
    Openai,
    /// Google Gemini through its OpenAI-compatible endpoint.
    Gemini,
    /// Claude on Amazon Bedrock with a Bedrock API key.
    Bedrock,
    /// Claude on Google Vertex AI; the daemon host's `gcloud` login supplies tokens.
    Vertex,
    /// `OpenRouter` (OpenAI-compatible).
    Openrouter,
    /// A local Ollama server (OpenAI-compatible, no key).
    Ollama,
    /// Any other OpenAI-compatible endpoint; needs `base_url`.
    OpenaiCompatible,
    /// `OpenAI` models through the Codex harness, with an API key or the
    /// daemon host's `ChatGPT` sign-in.
    Codex,
}

/// How the model proxy authenticates upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamAuth {
    /// `x-api-key: <key>`
    ApiKeyHeader,
    /// `Authorization: Bearer <key>` (omitted when no key is stored)
    Bearer,
    /// `Authorization: Bearer <gcloud access token>`
    GcloudToken,
}

impl ProviderKind {
    pub const ALL: [Self; 9] = [
        Self::Anthropic,
        Self::Openai,
        Self::Gemini,
        Self::Bedrock,
        Self::Vertex,
        Self::Openrouter,
        Self::Ollama,
        Self::OpenaiCompatible,
        Self::Codex,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Gemini => "gemini",
            Self::Bedrock => "bedrock",
            Self::Vertex => "vertex",
            Self::Openrouter => "openrouter",
            Self::Ollama => "ollama",
            Self::OpenaiCompatible => "openai_compatible",
            Self::Codex => "codex",
        }
    }

    /// Runner driver that speaks this provider's API.
    pub fn driver(self) -> &'static str {
        match self {
            Self::Anthropic | Self::Bedrock | Self::Vertex => "claude",
            Self::Codex => "codex",
            _ => "openai",
        }
    }

    pub fn upstream_auth(self) -> UpstreamAuth {
        match self {
            Self::Anthropic => UpstreamAuth::ApiKeyHeader,
            Self::Vertex => UpstreamAuth::GcloudToken,
            _ => UpstreamAuth::Bearer,
        }
    }

    /// Whether the provider is unusable without a stored key.
    pub fn needs_key(self) -> bool {
        !matches!(
            self,
            Self::Vertex | Self::Ollama | Self::OpenaiCompatible | Self::Codex
        )
    }

    /// Upstream base URL unless the provider overrides it. Bedrock and
    /// Vertex derive theirs from the region.
    pub fn default_base_url(self, region: Option<&str>) -> Option<String> {
        let fixed = match self {
            Self::Anthropic => "https://api.anthropic.com",
            Self::Openai | Self::Codex => "https://api.openai.com/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            Self::Openrouter => "https://openrouter.ai/api/v1",
            Self::Ollama => "http://127.0.0.1:11434/v1",
            Self::Bedrock => {
                return region.map(|r| format!("https://bedrock-runtime.{r}.amazonaws.com"));
            }
            Self::Vertex => {
                return region.map(|r| match r {
                    "global" => "https://aiplatform.googleapis.com/v1".to_owned(),
                    r => format!("https://{r}-aiplatform.googleapis.com/v1"),
                });
            }
            Self::OpenaiCompatible => return None,
        };
        Some(fixed.to_owned())
    }
}

impl FromStr for ProviderKind {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| CoreError::UnknownVariant {
                kind: "provider kind",
                value: s.to_owned(),
            })
    }
}

/// A configured model provider. Its credential lives in the daemon's secret
/// store, never here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct Provider {
    pub id: Uuid,
    /// Unique display name, e.g. `openai-work`.
    pub name: String,
    pub kind: ProviderKind,
    /// Upstream override; `null` uses the kind's default.
    pub base_url: Option<String>,
    /// Bedrock / Vertex region, e.g. `us-east-1`, `us-east5`, `global`.
    pub region: Option<String>,
    /// Google Cloud project (Vertex).
    pub project: Option<String>,
    /// Model for agents that do not pick one.
    pub default_model: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

impl Provider {
    /// Where the model proxy forwards to.
    pub fn upstream(&self) -> Option<String> {
        self.base_url
            .clone()
            .or_else(|| self.kind.default_base_url(self.region.as_deref()))
            .map(|u| u.trim_end_matches('/').to_owned())
    }

    /// Checks the fields the kind needs. Returns a human-readable reason.
    pub fn validate(&self) -> Result<(), String> {
        let blank = |v: &Option<String>| v.as_deref().is_none_or(|s| s.trim().is_empty());
        if self.name.trim().is_empty() || self.name.len() > 64 {
            return Err("name must be 1-64 bytes".into());
        }
        if let Some(url) = &self.base_url
            && !(url.starts_with("https://") || url.starts_with("http://"))
        {
            return Err("base_url must be an http(s) URL".into());
        }
        match self.kind {
            ProviderKind::OpenaiCompatible if blank(&self.base_url) => {
                Err("openai_compatible needs base_url".into())
            }
            ProviderKind::Bedrock if blank(&self.region) => Err("bedrock needs region".into()),
            ProviderKind::Vertex if blank(&self.region) || blank(&self.project) => {
                Err("vertex needs region and project".into())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(kind: ProviderKind) -> Provider {
        Provider {
            id: Uuid::nil(),
            name: "p".into(),
            kind,
            base_url: None,
            region: None,
            project: None,
            default_model: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn kinds_roundtrip() {
        for kind in ProviderKind::ALL {
            assert_eq!(kind.as_str().parse::<ProviderKind>().unwrap(), kind);
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
        assert!("grok".parse::<ProviderKind>().is_err());
    }

    #[test]
    fn upstreams() {
        let mut p = provider(ProviderKind::Bedrock);
        assert!(p.validate().is_err());
        p.region = Some("eu-central-1".into());
        p.validate().unwrap();
        assert_eq!(
            p.upstream().unwrap(),
            "https://bedrock-runtime.eu-central-1.amazonaws.com"
        );

        let mut v = provider(ProviderKind::Vertex);
        v.region = Some("global".into());
        assert!(v.validate().is_err(), "project missing");
        v.project = Some("proj".into());
        v.validate().unwrap();
        assert_eq!(
            v.upstream().unwrap(),
            "https://aiplatform.googleapis.com/v1"
        );

        let mut c = provider(ProviderKind::OpenaiCompatible);
        assert!(c.validate().is_err());
        c.base_url = Some("http://10.0.0.5:8000/v1/".into());
        c.validate().unwrap();
        assert_eq!(c.upstream().unwrap(), "http://10.0.0.5:8000/v1");
        c.base_url = Some("file:///etc".into());
        assert!(c.validate().is_err());

        assert_eq!(
            provider(ProviderKind::Gemini).upstream().unwrap(),
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
        assert_eq!(provider(ProviderKind::Gemini).kind.driver(), "openai");
        assert_eq!(provider(ProviderKind::Vertex).kind.driver(), "claude");
    }
}
