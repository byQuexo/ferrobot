//! Model prices for usage cost estimates: `config/pricing.toml`, with the
//! operator's overrides (setting `pricing`) on top.

use std::collections::HashMap;
use std::sync::LazyLock;

use ferrobot_core::ProviderKind;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Settings key of the operator's overrides (JSON list of [`PriceEntry`]).
pub const SETTING: &str = "pricing";

static FILE: LazyLock<Vec<PriceEntry>> = LazyLock::new(|| {
    parse_file(include_str!("../../../config/pricing.toml")).unwrap_or_else(|e| {
        tracing::error!(error = %e, "config/pricing.toml is invalid; no prices known");
        Vec::new()
    })
});

/// USD per 1M tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    /// `null`: unknown; calls with cache reads then cost an unknown amount.
    pub cache_read: Option<f64>,
    /// `null`: unknown; calls with cache writes then cost an unknown amount.
    pub cache_write: Option<f64>,
}

impl Price {
    const FREE: Self = Self {
        input: 0.0,
        output: 0.0,
        cache_read: Some(0.0),
        cache_write: Some(0.0),
    };

    pub fn valid(&self) -> bool {
        [
            Some(self.input),
            Some(self.output),
            self.cache_read,
            self.cache_write,
        ]
        .into_iter()
        .flatten()
        .all(|p| p.is_finite() && (0.0..=10_000.0).contains(&p))
    }
}

/// One model's price and where it comes from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PriceEntry {
    pub provider_kind: ProviderKind,
    /// API model id, as agents send it.
    pub model: String,
    #[serde(flatten)]
    pub price: Price,
    /// When the price was read off the source, or set by the operator (`YYYY-MM-DD`).
    pub checked: Option<String>,
    /// The provider's pricing page; `null` for the operator's own price.
    pub source: Option<String>,
}

fn parse_file(text: &str) -> Result<Vec<PriceEntry>, String> {
    let doc = toml_edit::Document::parse(text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (kind, models) in doc.as_table() {
        let provider_kind = kind.parse::<ProviderKind>().map_err(|e| e.to_string())?;
        let models = models
            .as_table()
            .ok_or_else(|| format!("{kind} is not a table"))?;
        for (model, entry) in models {
            let field = |name: &str| entry.get(name).and_then(|v| v.as_value());
            let price = |name: &str| field(name).and_then(toml_edit::Value::as_float);
            let text = |name: &str| {
                field(name)
                    .and_then(toml_edit::Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{kind}.{model}: {name} missing"))
            };
            let required =
                |name: &str| price(name).ok_or_else(|| format!("{kind}.{model}: {name} missing"));
            out.push(PriceEntry {
                provider_kind,
                model: model.to_owned(),
                price: Price {
                    input: required("input")?,
                    output: required("output")?,
                    cache_read: price("cache_read"),
                    cache_write: price("cache_write"),
                },
                checked: Some(text("checked")?),
                source: Some(text("source")?),
            });
        }
    }
    Ok(out)
}

/// The prices in effect: `overrides` first, then the file's entries they do not replace.
pub fn effective(overrides: Vec<PriceEntry>) -> Vec<PriceEntry> {
    let mut all = overrides;
    for entry in FILE.iter() {
        if !all
            .iter()
            .any(|o| o.provider_kind == entry.provider_kind && o.model == entry.model)
        {
            all.push(entry.clone());
        }
    }
    all.sort_by(|a, b| {
        (a.provider_kind.as_str(), &a.model).cmp(&(b.provider_kind.as_str(), &b.model))
    });
    all
}

/// A model id without a trailing Anthropic snapshot date (`-20250929`, `@20250929`).
fn undated(model: &str) -> &str {
    let b = model.as_bytes();
    let n = b.len();
    if n > 9 && matches!(b[n - 9], b'-' | b'@') && b[n - 8..].iter().all(u8::is_ascii_digit) {
        return &model[..n - 9];
    }
    model
}

/// Price lookup by provider kind and model.
#[derive(Debug)]
pub struct Prices(HashMap<(&'static str, String), Price>);

impl Prices {
    pub fn new(overrides: Vec<PriceEntry>) -> Self {
        Self(
            effective(overrides)
                .into_iter()
                .map(|e| ((e.provider_kind.as_str(), e.model), e.price))
                .collect(),
        )
    }

    pub fn get(&self, kind: &str, model: &str) -> Option<Price> {
        let kind = kind.parse::<ProviderKind>().ok()?;
        // Codex, Gemini CLI and Claude Code call the same models as OpenAI, Gemini and Anthropic;
        // a subscription covers Claude Code, so its cost is what the API would charge.
        let fallback = match kind {
            ProviderKind::Codex => ProviderKind::Openai,
            ProviderKind::GeminiCli => ProviderKind::Gemini,
            ProviderKind::ClaudeCode => ProviderKind::Anthropic,
            other => other,
        };
        // Anthropic prices a snapshot like its alias; OpenAI snapshots can differ from theirs.
        let dated = matches!(
            kind,
            ProviderKind::Anthropic
                | ProviderKind::Bedrock
                | ProviderKind::Vertex
                | ProviderKind::ClaudeCode
        );
        [kind, fallback]
            .into_iter()
            .find_map(|k| {
                [Some(model), dated.then(|| undated(model))]
                    .into_iter()
                    .flatten()
                    .find_map(|m| self.0.get(&(k.as_str(), m.to_owned())).copied())
            })
            .or((kind == ProviderKind::Ollama).then_some(Price::FREE))
    }
}

/// Estimated USD; `None` when tokens of a kind without a price were used.
#[allow(
    clippy::cast_precision_loss,
    reason = "token counts stay far below 2^52"
)]
pub fn cost(
    price: &Price,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
) -> Option<f64> {
    let part = |tokens: i64, rate: Option<f64>| {
        if tokens == 0 {
            Some(0.0)
        } else {
            rate.map(|r| tokens as f64 * r)
        }
    };
    let sum = part(input, Some(price.input))?
        + part(output, Some(price.output))?
        + part(cache_read, price.cache_read)?
        + part(cache_write, price.cache_write)?;
    Some(sum / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-9)
    }

    #[test]
    fn the_price_file_is_valid_and_sourced() {
        let entries = parse_file(include_str!("../../../config/pricing.toml")).unwrap();
        assert!(entries.len() > 20);
        for e in &entries {
            assert!(e.price.valid(), "{e:?}");
            assert_eq!(e.checked.as_deref(), Some("2026-11-20"), "{e:?}");
            assert!(
                e.source
                    .as_deref()
                    .is_some_and(|s| s.starts_with("https://")),
                "{e:?}"
            );
        }
        assert_eq!(FILE.len(), entries.len());
    }

    #[test]
    fn cost_counts_cache_tokens_at_their_own_price() {
        let p = Price {
            input: 3.0,
            output: 15.0,
            cache_read: Some(0.30),
            cache_write: Some(3.75),
        };
        // 1M input + 0.1M output + 2M cache reads + 0.4M cache writes
        assert!(close(
            cost(&p, 1_000_000, 100_000, 2_000_000, 400_000),
            3.0 + 1.5 + 0.6 + 1.5
        ));
        let no_cache = Price {
            cache_read: None,
            cache_write: None,
            ..p
        };
        assert!(close(cost(&no_cache, 10, 0, 0, 0), 0.000_03));
        assert_eq!(
            cost(&no_cache, 10, 0, 5, 0),
            None,
            "cache reads without a price"
        );
    }

    #[test]
    fn lookup_falls_back_and_overrides_win() {
        let own = |provider_kind, model: &str, price| PriceEntry {
            provider_kind,
            model: model.into(),
            price,
            checked: None,
            source: None,
        };
        let paid = Price {
            input: 1.0,
            ..Price::FREE
        };
        let prices = Prices::new(vec![
            own(ProviderKind::Openai, "gpt-5", Price::FREE),
            own(ProviderKind::Ollama, "llama-paid", paid),
            own(ProviderKind::Vertex, "claude-haiku-4-5", paid),
        ]);
        assert_eq!(
            prices.get("ollama", "llama-paid"),
            Some(paid),
            "an override beats free"
        );
        let sonnet = prices
            .get("anthropic", "claude-sonnet-4-5-20250929")
            .unwrap();
        assert!(
            (sonnet.input - 3.0).abs() < f64::EPSILON,
            "dated id uses the alias"
        );
        assert_eq!(
            prices.get("codex", "gpt-5"),
            Some(Price::FREE),
            "override, via openai"
        );
        assert_eq!(
            prices.get("claude_code", "claude-sonnet-4-5-20250929"),
            prices.get("anthropic", "claude-sonnet-4-5"),
            "Claude Code is priced like the Anthropic API"
        );
        assert!(prices.get("claude_code", "claude-sonnet-4-5").is_some());
        assert!(prices.get("gemini_cli", "gemini-2.5-flash").is_some());
        assert_eq!(prices.get("ollama", "llama9:70b"), Some(Price::FREE));
        assert_eq!(prices.get("anthropic", "claude-unknown-9"), None);
        assert_eq!(prices.get("bedrock", "anthropic.claude-opus-5"), None);
        assert_eq!(prices.get("nonsense", "gpt-5"), None);
        assert_eq!(
            prices.get("vertex", "claude-haiku-4-5@20251001"),
            Some(paid)
        );
        assert!(prices.get("openai", "gpt-4o").is_some());
        assert_eq!(
            prices.get("openai", "gpt-4o-2024-05-13"),
            None,
            "an OpenAI snapshot is not priced as its alias"
        );
        assert_eq!(undated("gpt-5"), "gpt-5");
    }
}
