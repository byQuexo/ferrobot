//! Model usage and estimated cost, from the rows the model proxy records.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use ferrobot_core::{AgentId, ProviderKind};
use ferrobot_store::{UsageFilter, UsageGroupBy, UsageSlice};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa::{IntoParams, ToSchema};

use super::{ApiResult, parse_id};
use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::limits::ProviderLimits;
use crate::pricing::{self, Price, PriceEntry, Prices};

const MAX_MODEL: usize = 200;
const MAX_OVERRIDES: usize = 1000;

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct UsageQuery {
    /// Start, inclusive (RFC 3339).
    from: Option<String>,
    /// End, exclusive (RFC 3339).
    to: Option<String>,
    /// Only this agent (id).
    agent: Option<String>,
    /// Only this provider (id), or `default` for the daemon's own key.
    provider: Option<String>,
    /// Only this model id.
    model: Option<String>,
    /// `day` (UTC date, the default), `agent`, `provider` or `model`.
    group_by: Option<String>,
    /// Also sum each group per day in the daemon's time zone (`days`).
    daily: Option<bool>,
}

/// Sums for a time range or group. Costs are what `OpenRouter` reported it charged, else
/// estimates from the price table.
#[derive(Debug, Default, Serialize, ToSchema)]
pub(crate) struct UsageTotals {
    calls: i64,
    /// Input tokens without cache reads and writes.
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    /// USD: `OpenRouter`'s reported charge where given, else estimated from prices; `null` when
    /// no call here could be priced.
    cost_usd: Option<f64>,
    /// Calls with tokens but no known price (not in `cost_usd`).
    unknown_cost_calls: i64,
    /// Successful calls whose API reported no tokens.
    not_tracked_calls: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct UsageGroup {
    /// UTC date, agent id, provider id (`default`: the daemon's key) or
    /// `<provider kind>/<model>`.
    key: String,
    /// Date, agent name, provider name or model id.
    label: String,
    #[serde(flatten)]
    totals: UsageTotals,
    /// Sums per day, oldest first, only days with calls; only with `daily=true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    days: Option<Vec<UsageDay>>,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct UsageDay {
    /// Date in the report's `timezone`, `YYYY-MM-DD`.
    date: String,
    #[serde(flatten)]
    totals: UsageTotals,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct UsageReport {
    totals: UsageTotals,
    groups: Vec<UsageGroup>,
    /// IANA time zone of the `days` dates (the daemon's `--timezone`); only
    /// with `daily=true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    timezone: Option<String>,
}

/// Running sums while slices are folded into a group.
#[derive(Default)]
struct Sum {
    totals: UsageTotals,
    cost: f64,
    priced: bool,
}

impl Sum {
    fn add(&mut self, s: &UsageSlice, prices: &Prices) {
        let t = &mut self.totals;
        t.calls += s.calls;
        t.input_tokens += s.input_tokens;
        t.output_tokens += s.output_tokens;
        t.cache_read_tokens += s.cache_read_tokens;
        t.cache_write_tokens += s.cache_write_tokens;
        t.not_tracked_calls += s.not_tracked_calls;
        if let Some(c) = s.reported_cost_usd {
            self.cost += c;
            self.priced = true;
            return;
        }
        if s.tracked_calls == 0 {
            return;
        }
        let cost = prices.get(&s.provider_kind, &s.model).and_then(|p| {
            pricing::cost(
                &p,
                s.input_tokens,
                s.output_tokens,
                s.cache_read_tokens,
                s.cache_write_tokens,
            )
        });
        match cost {
            Some(c) => {
                self.cost += c;
                self.priced = true;
            }
            None => t.unknown_cost_calls += s.tracked_calls,
        }
    }

    fn finish(self) -> UsageTotals {
        let mut t = self.totals;
        let unpriced = t.unknown_cost_calls + t.not_tracked_calls > 0;
        t.cost_usd = (self.priced || !unpriced).then_some(self.cost);
        t
    }
}

/// One group while slices are folded in.
struct GroupSum {
    key: String,
    label: String,
    /// Newest row id seen, so the latest label wins.
    last_id: i64,
    sum: Sum,
    days: BTreeMap<String, Sum>,
}

/// The date in `tz` of a UTC minute (`YYYY-MM-DDTHH:MM`).
fn local_date(minute: &str, tz: Tz) -> Result<String, ApiError> {
    let utc = NaiveDateTime::parse_from_str(minute, "%Y-%m-%dT%H:%M")
        .map_err(|e| ApiError::Internal(format!("usage time {minute}: {e}")))?;
    Ok(tz.from_utc_datetime(&utc).date_naive().to_string())
}

fn bad(message: &str) -> ApiError {
    ApiError::BadRequest(message.into())
}

fn time(raw: Option<&str>, field: &str) -> Result<Option<OffsetDateTime>, ApiError> {
    raw.map(|r| {
        OffsetDateTime::parse(r, &Rfc3339)
            .map_err(|_| bad(&format!("{field} must be an RFC 3339 time")))
    })
    .transpose()
}

fn filter(q: &UsageQuery) -> Result<UsageFilter, ApiError> {
    let (from, to) = (
        time(q.from.as_deref(), "from")?,
        time(q.to.as_deref(), "to")?,
    );
    if let (Some(from), Some(to)) = (from, to)
        && from > to
    {
        return Err(bad("from must not be after to"));
    }
    let agent = q
        .agent
        .as_deref()
        .map(|a| {
            a.parse::<AgentId>()
                .map_err(|_| bad("agent must be an agent id"))
        })
        .transpose()?;
    // Stored ids are lowercase and hyphenated; the query may spell them otherwise.
    let provider = q
        .provider
        .as_deref()
        .map(|p| match p {
            "default" => Ok(p.to_owned()),
            _ => p
                .parse::<uuid::Uuid>()
                .map(|id| id.to_string())
                .map_err(|_| bad("provider must be a provider id or default")),
        })
        .transpose()?;
    if q.model.as_deref().is_some_and(|m| m.len() > MAX_MODEL) {
        return Err(bad("model is too long"));
    }
    Ok(UsageFilter {
        from,
        to,
        agent,
        provider,
        model: q.model.clone(),
    })
}

async fn overrides(state: &AppState) -> Result<Vec<PriceEntry>, ApiError> {
    match state.store.get_setting(pricing::SETTING).await? {
        Some(raw) => serde_json::from_str(&raw)
            .map_err(|e| ApiError::Internal(format!("price overrides: {e}"))),
        None => Ok(Vec::new()),
    }
}

/// Calls, tokens and estimated cost of the model calls agents made through
/// the proxy, in total and per group.
#[utoipa::path(get, path = "/v1/usage", tag = "usage",
    params(UsageQuery),
    responses((status = 200, body = UsageReport),
        (status = 400, description = "Bad filter", body = ErrorBody)))]
pub(crate) async fn get_usage(
    State(state): State<AppState>,
    Query(q): Query<UsageQuery>,
) -> ApiResult<UsageReport> {
    let group_by = match q.group_by.as_deref().unwrap_or("day") {
        "day" => UsageGroupBy::Day,
        "agent" => UsageGroupBy::Agent,
        "provider" => UsageGroupBy::Provider,
        "model" => UsageGroupBy::Model,
        _ => return Err(bad("group_by must be day, agent, provider or model")),
    };
    let zone = if q.daily.unwrap_or(false) {
        let name = &state.settings.default_timezone;
        let tz: Tz = name
            .parse()
            .map_err(|_| ApiError::Internal(format!("unknown time zone {name:?}")))?;
        Some((tz, name.clone()))
    } else {
        None
    };
    let slices = state
        .store
        .usage_slices(filter(&q)?, group_by, zone.is_some())
        .await?;
    let prices = Prices::new(overrides(&state).await?);
    let mut total = Sum::default();
    // Slices come ordered by key.
    let mut groups: Vec<GroupSum> = Vec::new();
    for s in &slices {
        total.add(s, &prices);
        if groups.last().is_none_or(|g| g.key != s.key) {
            groups.push(GroupSum {
                key: s.key.clone(),
                label: s.label.clone(),
                last_id: s.last_id,
                sum: Sum::default(),
                days: BTreeMap::new(),
            });
        }
        if let Some(g) = groups.last_mut() {
            if s.last_id > g.last_id {
                (g.label, g.last_id) = (s.label.clone(), s.last_id);
            }
            g.sum.add(s, &prices);
            if let (Some((tz, _)), Some(minute)) = (&zone, &s.minute) {
                g.days
                    .entry(local_date(minute, *tz)?)
                    .or_default()
                    .add(s, &prices);
            }
        }
    }
    Ok(Json(UsageReport {
        totals: total.finish(),
        groups: groups
            .into_iter()
            .map(|g| UsageGroup {
                key: g.key,
                label: g.label,
                totals: g.sum.finish(),
                days: zone.is_some().then(|| {
                    g.days
                        .into_iter()
                        .map(|(date, sum)| UsageDay {
                            date,
                            totals: sum.finish(),
                        })
                        .collect()
                }),
            })
            .collect(),
        timezone: zone.map(|(_, name)| name),
    }))
}

/// How much of their 5-hour and weekly plan limits the host's Claude Code login and each Codex
/// provider signed in with `ChatGPT` have used, read from the CLIs (cached for a minute).
#[utoipa::path(get, path = "/v1/limits", tag = "usage",
    responses((status = 200, body = Vec<ProviderLimits>)))]
pub(crate) async fn get_limits(State(state): State<AppState>) -> ApiResult<Vec<ProviderLimits>> {
    Ok(Json(crate::limits::all(&state).await?))
}

/// Prices in USD per 1M tokens: the operator's overrides, then the shipped
/// table. Unlisted Ollama models cost 0.
#[utoipa::path(get, path = "/v1/usage/prices", tag = "usage",
    responses((status = 200, body = Vec<PriceEntry>)))]
pub(crate) async fn get_prices(State(state): State<AppState>) -> ApiResult<Vec<PriceEntry>> {
    Ok(Json(pricing::effective(overrides(&state).await?)))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct SetPrice {
    provider_kind: ProviderKind,
    /// API model id, as agents send it.
    model: String,
    #[serde(flatten)]
    price: Price,
}

/// Replaces the operator's price overrides (the whole list) and returns the
/// prices in effect.
#[utoipa::path(put, path = "/v1/usage/prices", tag = "usage",
    request_body = Vec<SetPrice>,
    responses((status = 200, body = Vec<PriceEntry>),
        (status = 400, body = ErrorBody)))]
pub(crate) async fn put_prices(
    State(state): State<AppState>,
    Json(req): Json<Vec<SetPrice>>,
) -> ApiResult<Vec<PriceEntry>> {
    if req.len() > MAX_OVERRIDES {
        return Err(bad("too many prices"));
    }
    let old = overrides(&state).await?;
    let today = OffsetDateTime::now_utc().date().to_string();
    let mut new: Vec<PriceEntry> = Vec::with_capacity(req.len());
    for p in req {
        let model = p.model.trim().to_owned();
        if model.is_empty() || model.len() > MAX_MODEL || model.chars().any(char::is_control) {
            return Err(bad("model must be 1-200 bytes"));
        }
        if !p.price.valid() {
            return Err(bad("prices must be between 0 and 10000"));
        }
        let same = |e: &PriceEntry| e.provider_kind == p.provider_kind && e.model == model;
        if new.iter().any(same) {
            return Err(bad(&format!("{model} is listed twice")));
        }
        // An unchanged override keeps the date it was set.
        let checked = old
            .iter()
            .find(|e| same(e) && e.price == p.price)
            .and_then(|e| e.checked.clone())
            .unwrap_or_else(|| today.clone());
        new.push(PriceEntry {
            provider_kind: p.provider_kind,
            model,
            price: p.price,
            checked: Some(checked),
            source: None,
        });
    }
    let raw = serde_json::to_string(&new).map_err(|e| ApiError::Internal(e.to_string()))?;
    state.store.put_setting(pricing::SETTING, raw).await?;
    Ok(Json(pricing::effective(new)))
}

/// How full an agent's context is, read from its newest proxied model call.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct AgentContext {
    /// Whether the daemon sees the agent's model calls: `false` on Claude Code, which runs on
    /// the host's own login and not through the model proxy.
    tracked: bool,
    /// Input plus cache read and write tokens of the newest call since the last new chat;
    /// `null` before there is one.
    tokens: Option<i64>,
    /// Context window of `model` in tokens; `null` when not known.
    window: Option<i64>,
    /// The model of that call, else the agent's configured model.
    model: Option<String>,
    /// When that call was made.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    at: Option<OffsetDateTime>,
}

/// How full the agent's context is.
#[utoipa::path(get, path = "/v1/agents/{id}/context", tag = "usage",
    params(("id" = String, Path, description = "Agent id (UUID)")),
    responses((status = 200, body = AgentContext),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn get_context(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<AgentContext> {
    let agent = state.store.get_agent(parse_id(&id)?).await?;
    let provider = match agent.provider_id {
        Some(p) => state.store.get_provider(p).await.ok(),
        None => None,
    };
    if provider
        .as_ref()
        .is_some_and(|p| p.kind == ProviderKind::ClaudeCode)
    {
        return Ok(Json(AgentContext {
            tracked: false,
            tokens: None,
            window: None,
            model: None,
            at: None,
        }));
    }
    // shortcut: an agent on its provider's default can show a side call on a smaller model; name the driver's main model here if that misleads.
    let configured = agent
        .model
        .or_else(|| provider.and_then(|p| p.default_model));
    let usage = state
        .store
        .context_usage(agent.id, configured.clone())
        .await?;
    let model = usage.as_ref().map(|u| u.model.clone()).or(configured);
    Ok(Json(AgentContext {
        tracked: true,
        tokens: usage.as_ref().map(|u| u.tokens),
        window: model.as_deref().and_then(context_window),
        model,
        at: usage.map(|u| u.at),
    }))
}

/// Context window in tokens of a model id as providers write it (with `anthropic.`, `models/` or
/// `vendor/` prefixes); `None` when not known.
// shortcut: a hand-kept table of model families; read windows from the providers' model lists if it falls behind.
fn context_window(model: &str) -> Option<i64> {
    let m = model.to_ascii_lowercase();
    let id = m.rsplit('/').next().unwrap_or(&m);
    if m.contains("[1m]") || m.ends_with("-1m") {
        Some(1_000_000)
    } else if id.contains("claude") {
        Some(if claude_has_1m(id) {
            1_000_000
        } else {
            200_000
        })
    } else if id.contains("gemini") {
        Some(1_048_576)
    } else if id.starts_with("gpt-4.1") {
        Some(1_047_576)
    } else if id.starts_with("gpt-5") {
        Some(400_000)
    } else if id.starts_with("gpt-4o") || id.starts_with("gpt-oss") {
        Some(128_000)
    } else if ["o1", "o3", "o4"].iter().any(|p| id.starts_with(p)) {
        Some(200_000)
    } else {
        None
    }
}

/// Fable, Mythos and Opus, Sonnet or Haiku 4.6 and later have 1M tokens; older Claude 200K.
fn claude_has_1m(id: &str) -> bool {
    let words: Vec<&str> = id.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    if words.iter().any(|w| matches!(*w, "fable" | "mythos")) {
        return true;
    }
    let Some(family) = words
        .iter()
        .position(|w| matches!(*w, "opus" | "sonnet" | "haiku"))
    else {
        return false;
    };
    // A date like `20250514` is not a version number.
    let version = |i: usize| {
        words
            .get(family + i)
            .and_then(|w| w.parse::<u32>().ok())
            .filter(|n| *n < 100)
    };
    version(1).is_some_and(|major| major > 4 || (major == 4 && version(2).unwrap_or(0) >= 6))
}

#[cfg(test)]
mod tests {
    use super::context_window;

    #[test]
    fn context_windows_by_model_family() {
        let cases = [
            ("claude-opus-5-5", Some(1_000_000)),
            ("claude-sonnet-4-6", Some(1_000_000)),
            ("us.anthropic.claude-opus-4-6-v1:0", Some(1_000_000)),
            ("claude-fable-5-2", Some(1_000_000)),
            ("claude-haiku-4-5-20251001", Some(200_000)),
            ("claude-sonnet-4-20250514", Some(200_000)),
            ("anthropic/claude-sonnet-4.5", Some(200_000)),
            ("claude-sonnet-4-5[1m]", Some(1_000_000)),
            ("claude-3-5-sonnet-20241022", Some(200_000)),
            ("gemini-2.5-pro", Some(1_048_576)),
            ("openai/gpt-5.1-codex", Some(400_000)),
            ("gpt-4o-mini", Some(128_000)),
            ("o4-mini", Some(200_000)),
            ("tencent/hy4-flash", None),
            ("llama3.2", None),
        ];
        for (model, window) in cases {
            assert_eq!(context_window(model), window, "{model}");
        }
    }
}
