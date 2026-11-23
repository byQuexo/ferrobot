//! Model usage and estimated cost, from the rows the model proxy records.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Query, State};
use chrono::{NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use ferrobot_core::{AgentId, ProviderKind};
use ferrobot_store::{UsageFilter, UsageGroupBy, UsageSlice};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa::{IntoParams, ToSchema};

use super::ApiResult;
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
