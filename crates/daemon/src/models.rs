//! Model lists for the pickers: the provider's own list where its API has one, else a short
//! built-in list per kind. The key goes only into the request to the provider.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ferrobot_core::{Provider, ProviderKind};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::AppState;
use crate::proxy::secret_key;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
/// `OpenRouter`'s list with descriptions is a few MB; a server sending more is refused.
const MAX_LIST_BYTES: usize = 8 * 1024 * 1024;

/// One model a picker offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ModelInfo {
    /// The id an agent or provider uses as its `model`.
    pub id: String,
    /// Display name, when the provider gives one.
    pub name: Option<String>,
}

/// Where a model list comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// Fetched from the provider with its stored key.
    Provider,
    /// ferrobot's short built-in list for the kind.
    Suggested,
}

/// Models to offer for a provider. Any other id can still be typed.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelList {
    pub models: Vec<ModelInfo>,
    pub source: ModelSource,
}

/// Provider lists fetched in the last few minutes, by provider id.
pub type ModelCache = Mutex<HashMap<Uuid, (Instant, Vec<ModelInfo>)>>;

// Checked 2026-11-20: platform.claude.com/docs/en/about-claude/models/overview (+ Bedrock, Vertex pages),
// developers.openai.com/api/docs/models, learn.chatgpt.com/docs/models, aistudio.google.com/docs/models, openrouter.ai, ollama.com/library.
fn suggested_ids(kind: ProviderKind) -> &'static [&'static str] {
    match kind {
        ProviderKind::Anthropic | ProviderKind::Vertex => &[
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-fable-5-2",
        ],
        ProviderKind::Bedrock => &[
            "global.anthropic.claude-opus-5-5",
            "global.anthropic.claude-sonnet-5-5",
            "global.anthropic.claude-haiku-5-5",
            "global.anthropic.claude-fable-5-2",
        ],
        ProviderKind::Openai => &[
            "gpt-6.2-astra",
            "gpt-6.2-sol",
            "gpt-6.1-terra",
            "gpt-6-luna",
        ],
        ProviderKind::Codex => &["gpt-6.2-astra", "gpt-6.2-sol", "gpt-6.1-sol", "gpt-6-luna"],
        ProviderKind::Gemini | ProviderKind::GeminiCli => &[
            "gemini-4-carbon",
            "gemini-4-neon",
            "gemini-4-hydrogen",
            "gemini-3.8-flash",
        ],
        ProviderKind::Openrouter => &[
            "anthropic/claude-opus-5.5",
            "anthropic/claude-sonnet-5.5",
            "openai/gpt-6.2-sol",
            "google/gemini-4-neon",
        ],
        ProviderKind::Ollama => &["gemma3", "qwen3", "llama3.1", "deepseek-r1"],
        ProviderKind::OpenaiCompatible => &[],
        // Claude Code's own aliases; its picker knows the full ids.
        ProviderKind::ClaudeCode => &["opus", "sonnet", "haiku"],
    }
}

/// The built-in list for a kind.
pub fn suggested(kind: ProviderKind) -> ModelList {
    ModelList {
        models: suggested_ids(kind)
            .iter()
            .map(|id| ModelInfo {
                id: (*id).to_owned(),
                name: None,
            })
            .collect(),
        source: ModelSource::Suggested,
    }
}

/// The provider's own list, or the kind's suggestions when it has none or the fetch fails.
pub async fn list(state: &AppState, provider: &Provider) -> ModelList {
    let cached = crate::lock(&state.model_lists)
        .get(&provider.id)
        .filter(|(at, _)| at.elapsed() < CACHE_TTL)
        .map(|(_, models)| models.clone());
    if let Some(models) = cached {
        return ModelList {
            models,
            source: ModelSource::Provider,
        };
    }
    match fetch(state, provider).await {
        Ok(models) if !models.is_empty() => {
            crate::lock(&state.model_lists).insert(provider.id, (Instant::now(), models.clone()));
            ModelList {
                models,
                source: ModelSource::Provider,
            }
        }
        Ok(_) => suggested(provider.kind),
        Err(reason) => {
            tracing::info!(provider = %provider.name, %reason, "no model list from the provider, suggesting models");
            suggested(provider.kind)
        }
    }
}

/// Drops a provider's cached list, e.g. after its key or URL changed.
pub fn forget(state: &AppState, provider: Uuid) {
    crate::lock(&state.model_lists).remove(&provider);
}

/// The list endpoint below the provider's upstream, as the proxy reaches it.
fn list_url(provider: &Provider) -> Option<String> {
    let base = provider.upstream()?;
    match provider.kind {
        ProviderKind::Anthropic => Some(format!("{base}/v1/models?limit=1000")),
        ProviderKind::Openai
        | ProviderKind::Gemini
        | ProviderKind::Openrouter
        | ProviderKind::Ollama
        | ProviderKind::OpenaiCompatible
        | ProviderKind::Codex => Some(format!("{base}/models")),
        ProviderKind::Bedrock
        | ProviderKind::Vertex
        | ProviderKind::GeminiCli
        | ProviderKind::ClaudeCode => None,
    }
}

async fn fetch(state: &AppState, provider: &Provider) -> Result<Vec<ModelInfo>, String> {
    let url = list_url(provider).ok_or("the provider has no model list endpoint")?;
    let key = state
        .settings
        .secrets
        .get(&secret_key(provider.id))
        .await
        .map_err(|e| e.to_string())?;
    // A Codex provider without a key runs on the host's ChatGPT sign-in, which has no list.
    if key.is_none() && (provider.kind.needs_key() || provider.kind == ProviderKind::Codex) {
        return Err("no API key stored".into());
    }
    let mut request = state.http.get(&url).timeout(FETCH_TIMEOUT);
    if let Some(key) = key {
        request = if provider.kind == ProviderKind::Anthropic {
            request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
        } else {
            request.bearer_auth(key)
        };
    }
    let response = request
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    // Error bodies can echo part of the key, so only the status is kept.
    if !response.status().is_success() {
        return Err(format!("status {}", response.status()));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.without_url().to_string())?;
        if body.len() + chunk.len() > MAX_LIST_BYTES {
            return Err("model list too large".into());
        }
        body.extend_from_slice(&chunk);
    }
    // serde's message can quote a body string, so it is not kept either.
    parse(provider.kind, &body).map_err(|_| "unexpected model list".to_owned())
}

#[derive(Deserialize)]
struct Listing {
    // shortcut: only the first page (OpenRouter pages with `links.next`), follow it if lists get cut off.
    data: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    #[serde(default, alias = "display_name")]
    name: Option<String>,
}

/// Anthropic, `OpenAI`, `OpenRouter`, Ollama and Gemini all answer with `{"data": [{"id", ...}]}`.
fn parse(kind: ProviderKind, body: &[u8]) -> Result<Vec<ModelInfo>, serde_json::Error> {
    let listing: Listing = serde_json::from_slice(body)?;
    Ok(listing
        .data
        .into_iter()
        .filter_map(|entry| {
            let id = match kind {
                ProviderKind::Gemini => entry.id.strip_prefix("models/").unwrap_or(&entry.id),
                _ => &entry.id,
            };
            (!id.is_empty()).then(|| ModelInfo {
                id: id.to_owned(),
                name: entry.name.filter(|n| !n.is_empty()),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_providers_list() {
        let info = |id: &str, name: Option<&str>| ModelInfo {
            id: id.into(),
            name: name.map(Into::into),
        };
        let cases: [(ProviderKind, &str, Vec<ModelInfo>); 5] = [
            (
                ProviderKind::Anthropic,
                r#"{"data":[{"type":"model","id":"claude-opus-5-5","display_name":"Claude Opus 5.5",
                    "created_at":"2026-09-21T00:00:00Z","line":"opus"}],
                    "has_more":false,"first_id":"claude-opus-5-5","last_id":"claude-opus-5-5"}"#,
                vec![info("claude-opus-5-5", Some("Claude Opus 5.5"))],
            ),
            (
                ProviderKind::Openai,
                r#"{"object":"list","data":[{"id":"gpt-6.2-sol","object":"model","created":1,"owned_by":"openai"},
                    {"id":"","object":"model"}]}"#,
                vec![info("gpt-6.2-sol", None)],
            ),
            (
                ProviderKind::Openrouter,
                r#"{"data":[{"id":"openai/gpt-4","canonical_slug":"openai/gpt-4","name":"GPT-4",
                    "created":1,"context_length":8192,"pricing":{"prompt":"0.00003"}}],
                    "links":{"next":null},"total_count":1}"#,
                vec![info("openai/gpt-4", Some("GPT-4"))],
            ),
            (
                ProviderKind::Ollama,
                r#"{"object":"list","data":[{"id":"qwen3:latest","object":"model","created":1,"owned_by":"library"}]}"#,
                vec![info("qwen3:latest", None)],
            ),
            (
                ProviderKind::Gemini,
                r#"{"object":"list","data":[{"id":"models/gemini-4-neon","object":"model","owned_by":"google"},
                    {"id":"gemini-3.8-flash","object":"model","owned_by":"google"}]}"#,
                vec![info("gemini-4-neon", None), info("gemini-3.8-flash", None)],
            ),
        ];
        for (kind, body, want) in cases {
            assert_eq!(parse(kind, body.as_bytes()).unwrap(), want, "{kind:?}");
        }
        assert!(parse(ProviderKind::Openai, b"<html>").is_err());
        assert!(parse(ProviderKind::Openai, br#"{"models":[]}"#).is_err());
    }
}
