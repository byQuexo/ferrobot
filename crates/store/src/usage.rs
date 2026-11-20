//! Model usage: one row per proxied model call, counts and metadata only.

use ferrobot_core::AgentId;
use rusqlite::params;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::util::fmt_ts;
use crate::{Store, StoreError};

/// How a proxied call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageStatus {
    /// The upstream answered with 2xx and its body ended normally.
    Ok,
    /// The proxy refused the call before it reached the upstream.
    Refused,
    /// The upstream answered with an error, could not be reached, or its
    /// body broke off, carried an error event or was dropped by the agent.
    Error,
}

impl UsageStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Refused => "refused",
            Self::Error => "error",
        }
    }
}

/// Token counts as the API reported them; `None` = not reported. `input`
/// excludes cache reads and writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_read: Option<i64>,
    pub cache_write: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct UsageRecord {
    pub at: OffsetDateTime,
    pub agent_id: AgentId,
    pub agent_name: String,
    /// `None`: the daemon's own `ANTHROPIC_API_KEY`.
    pub provider_id: Option<Uuid>,
    pub provider_kind: String,
    pub model: String,
    pub tokens: Tokens,
    /// USD the upstream said it charged (`OpenRouter`); `None`: estimate from prices.
    pub cost_usd: Option<f64>,
    pub latency_ms: i64,
    pub status: UsageStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageGroupBy {
    /// UTC date, `YYYY-MM-DD`.
    Day,
    Agent,
    /// Provider id, or `default` for the daemon's own key.
    Provider,
    /// `<provider kind>/<model>`.
    Model,
}

/// Which rows to count; `None` matches everything. Times are `[from, to)`.
#[derive(Debug, Clone, Default)]
pub struct UsageFilter {
    pub from: Option<OffsetDateTime>,
    pub to: Option<OffsetDateTime>,
    pub agent: Option<AgentId>,
    /// Provider id, or `default` for the daemon's own key.
    pub provider: Option<String>,
    pub model: Option<String>,
}

/// Sums for one group and one (provider kind, model), so prices can apply.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageSlice {
    pub key: String,
    pub label: String,
    pub provider_kind: String,
    pub model: String,
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    /// Calls that reported input or output tokens.
    pub tracked_calls: i64,
    /// Successful calls that reported no tokens.
    pub not_tracked_calls: i64,
    /// Newest row id, so the latest label wins when names change.
    pub last_id: i64,
    /// Sum of the upstream-reported costs; a slice holds either only such rows or none.
    pub reported_cost_usd: Option<f64>,
}

/// Whole seconds keep the stored RFC 3339 strings the same width, so they sort as times.
fn ts(at: OffsetDateTime) -> String {
    fmt_ts(
        at.to_offset(time::UtcOffset::UTC)
            .replace_nanosecond(0)
            .unwrap_or(at),
    )
}

impl Store {
    pub async fn insert_usage(&self, r: UsageRecord) -> Result<(), StoreError> {
        self.with(move |c| {
            c.execute(
                "INSERT INTO usage (at, agent_id, agent_name, provider_id, provider_kind, model,
                                    input_tokens, output_tokens, cache_read_tokens,
                                    cache_write_tokens, latency_ms, status, cost_usd)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    ts(r.at),
                    r.agent_id.to_string(),
                    r.agent_name,
                    r.provider_id.map(|p| p.to_string()),
                    r.provider_kind,
                    r.model,
                    r.tokens.input,
                    r.tokens.output,
                    r.tokens.cache_read,
                    r.tokens.cache_write,
                    r.latency_ms,
                    r.status.as_str(),
                    r.cost_usd
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Usage summed per group and (provider kind, model), ordered by key.
    pub async fn usage_slices(
        &self,
        filter: UsageFilter,
        group_by: UsageGroupBy,
    ) -> Result<Vec<UsageSlice>, StoreError> {
        let (key, label) = match group_by {
            UsageGroupBy::Day => ("substr(u.at, 1, 10)", "substr(u.at, 1, 10)"),
            UsageGroupBy::Agent => ("u.agent_id", "u.agent_name"),
            UsageGroupBy::Provider => (
                "COALESCE(u.provider_id, 'default')",
                "CASE WHEN u.provider_id IS NULL THEN 'Daemon key'
                      ELSE COALESCE(p.name, u.provider_kind) END",
            ),
            UsageGroupBy::Model => ("u.provider_kind || '/' || u.model", "u.model"),
        };
        // The label is a bare column: SQLite takes it from the MAX(u.id) row.
        let sql = format!(
            "SELECT {key}, {label}, u.provider_kind, u.model, COUNT(*),
                    COALESCE(SUM(u.input_tokens), 0), COALESCE(SUM(u.output_tokens), 0),
                    COALESCE(SUM(u.cache_read_tokens), 0), COALESCE(SUM(u.cache_write_tokens), 0),
                    SUM(u.input_tokens IS NOT NULL OR u.output_tokens IS NOT NULL),
                    SUM(u.status = 'ok' AND u.input_tokens IS NULL AND u.output_tokens IS NULL),
                    MAX(u.id), SUM(u.cost_usd)
             FROM usage u LEFT JOIN providers p ON p.id = u.provider_id
             WHERE (?1 IS NULL OR u.at >= ?1) AND (?2 IS NULL OR u.at < ?2)
               AND (?3 IS NULL OR u.agent_id = ?3)
               AND (?4 IS NULL OR COALESCE(u.provider_id, 'default') = ?4)
               AND (?5 IS NULL OR u.model = ?5)
             GROUP BY 1, u.provider_kind, u.model, u.cost_usd IS NULL
             ORDER BY 1, u.provider_kind, u.model, u.cost_usd IS NULL"
        );
        self.with(move |c| {
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(
                params![
                    filter.from.map(ts),
                    filter.to.map(ts),
                    filter.agent.map(|a| a.to_string()),
                    filter.provider,
                    filter.model
                ],
                |r| {
                    Ok(UsageSlice {
                        key: r.get(0)?,
                        label: r.get(1)?,
                        provider_kind: r.get(2)?,
                        model: r.get(3)?,
                        calls: r.get(4)?,
                        input_tokens: r.get(5)?,
                        output_tokens: r.get(6)?,
                        cache_read_tokens: r.get(7)?,
                        cache_write_tokens: r.get(8)?,
                        tracked_calls: r.get(9)?,
                        not_tracked_calls: r.get(10)?,
                        last_id: r.get(11)?,
                        reported_cost_usd: r.get(12)?,
                    })
                },
            )?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::parse_ts;

    fn record(at: OffsetDateTime, agent: AgentId, model: &str, input: Option<i64>) -> UsageRecord {
        UsageRecord {
            at,
            agent_id: agent,
            agent_name: "ada".into(),
            provider_id: None,
            provider_kind: "anthropic".into(),
            model: model.into(),
            tokens: Tokens {
                input,
                output: input.map(|_| 5),
                cache_read: Some(7),
                cache_write: None,
            },
            cost_usd: None,
            latency_ms: 12,
            status: UsageStatus::Ok,
        }
    }

    #[tokio::test]
    async fn reported_costs_are_summed_apart_from_estimates() {
        let store = Store::open_in_memory().unwrap();
        let (a, at) = (AgentId::new(), parse_ts("2026-11-20T08:00:00Z").unwrap());
        for cost in [None, Some(0.25), Some(0.5)] {
            let row = UsageRecord {
                cost_usd: cost,
                ..record(at, a, "m1", Some(10))
            };
            store.insert_usage(row).await.unwrap();
        }
        let slices = store
            .usage_slices(UsageFilter::default(), UsageGroupBy::Model)
            .await
            .unwrap();
        let split: Vec<_> = slices
            .iter()
            .map(|s| (s.calls, s.reported_cost_usd))
            .collect();
        assert_eq!(split, [(2, Some(0.75)), (1, None)]);
    }

    #[tokio::test]
    async fn usage_sums_by_group_and_model() {
        let store = Store::open_in_memory().unwrap();
        let (a, b) = (AgentId::new(), AgentId::new());
        let late = parse_ts("2026-11-19T23:59:59.5Z").unwrap();
        let next = parse_ts("2026-11-20T08:00:00+01:00").unwrap();
        store
            .insert_usage(record(late, a, "m1", Some(10)))
            .await
            .unwrap();
        store
            .insert_usage(record(next, a, "m1", Some(20)))
            .await
            .unwrap();
        store
            .insert_usage(record(next, b, "m2", None))
            .await
            .unwrap();

        let days = store
            .usage_slices(UsageFilter::default(), UsageGroupBy::Day)
            .await
            .unwrap();
        let keys: Vec<_> = days
            .iter()
            .map(|s| (s.key.as_str(), s.model.as_str()))
            .collect();
        assert_eq!(
            keys,
            [
                ("2026-11-19", "m1"),
                ("2026-11-20", "m1"),
                ("2026-11-20", "m2")
            ]
        );
        assert_eq!((days[1].input_tokens, days[1].cache_read_tokens), (20, 7));
        assert_eq!((days[2].tracked_calls, days[2].not_tracked_calls), (0, 1));

        let only_a = UsageFilter {
            from: Some(parse_ts("2026-11-20T00:00:00Z").unwrap()),
            agent: Some(a),
            ..UsageFilter::default()
        };
        let agents = store
            .usage_slices(only_a, UsageGroupBy::Agent)
            .await
            .unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!((agents[0].calls, agents[0].input_tokens), (1, 20));

        let before = UsageFilter {
            to: Some(parse_ts("2026-11-20T07:00:00Z").unwrap()),
            ..UsageFilter::default()
        };
        let models = store
            .usage_slices(before, UsageGroupBy::Model)
            .await
            .unwrap();
        assert_eq!(models[0].key, "anthropic/m1");
        assert_eq!(models[0].calls, 1, "[from, to) excludes the later rows");
    }
}
