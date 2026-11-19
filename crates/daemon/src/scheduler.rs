//! Cron schedules for routines and the loop that fires them.
//!
//! Schedules are standard 5-field cron expressions (minute granularity) or
//! aliases such as `@hourly`, evaluated in the routine's IANA time zone, so
//! "0 9 * * 1-5" means 09:00 local time on weekdays across DST changes.
//! A routine that came due while the daemon was down fires once on startup.

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;
use croner::parser::{CronParser, Seconds};
use ferrobot_store::{Routine, RunTrigger};
use time::OffsetDateTime;

use crate::error::ApiError;
use crate::{AppState, UiEvent};

/// Longest sleep between checks, so clock jumps are noticed.
/// Pause after a failed pass.
const ERROR_BACKOFF: Duration = Duration::from_secs(5);
const MAX_IDLE: Duration = Duration::from_secs(30);

/// A parsed, validated schedule.
#[derive(Debug, Clone)]
pub struct Schedule {
    cron: Cron,
    tz: Tz,
}

impl Schedule {
    pub fn parse(expr: &str, timezone: &str) -> Result<Self, ApiError> {
        let tz: Tz = timezone
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("unknown time zone {timezone:?}")))?;
        let expr = expr.trim();
        if expr.is_empty() || expr.len() > 100 {
            return Err(ApiError::BadRequest(
                "schedule must be 1-100 characters".into(),
            ));
        }
        let cron = CronParser::builder()
            .seconds(Seconds::Disallowed)
            .build()
            .parse(expr)
            .map_err(|e| ApiError::BadRequest(format!("invalid schedule {expr:?}: {e}")))?;
        Ok(Self { cron, tz })
    }

    /// First occurrence strictly after `after`.
    pub fn next_after(&self, after: OffsetDateTime) -> Option<OffsetDateTime> {
        let start = Utc
            .timestamp_opt(after.unix_timestamp(), 0)
            .single()?
            .with_timezone(&self.tz);
        let next: DateTime<Tz> = self.cron.find_next_occurrence(&start, false).ok()?;
        OffsetDateTime::from_unix_timestamp(next.timestamp()).ok()
    }

    /// Human-readable English description, e.g. "At 09:00 on Monday through Friday".
    pub fn describe(&self) -> String {
        self.cron.describe()
    }
}

/// The next run of a routine given its current settings (`None` when disabled).
pub fn next_run(
    schedule: &str,
    timezone: &str,
    enabled: bool,
) -> Result<Option<OffsetDateTime>, ApiError> {
    let parsed = Schedule::parse(schedule, timezone)?;
    Ok(enabled
        .then(|| parsed.next_after(OffsetDateTime::now_utc()))
        .flatten())
}

/// Fires one routine now and notifies everyone involved.
pub async fn fire(
    state: &AppState,
    routine: &Routine,
    trigger: RunTrigger,
) -> Result<ferrobot_core::MailMessage, ApiError> {
    let next = match trigger {
        RunTrigger::Manual => routine.next_run_at,
        RunTrigger::Schedule => Schedule::parse(&routine.schedule, &routine.timezone)
            .ok()
            .and_then(|s| s.next_after(OffsetDateTime::now_utc())),
    };
    let (_, msg) = state.store.fire_routine(routine.id, trigger, next).await?;
    tracing::info!(routine = %routine.name, agent = %routine.agent_id, ?trigger, "routine fired");
    state.message_arrived(routine.agent_id);
    state.publish_message(msg.id).await;
    state.publish(UiEvent::RoutinesChanged {
        agent_id: routine.agent_id,
    });
    Ok(msg)
}

/// Runs forever: fires due routines, then sleeps until the next one is due.
pub async fn run(state: AppState) {
    loop {
        let now = OffsetDateTime::now_utc();
        // After a failure, back off instead of spinning on a routine that stays due.
        let mut floor = Duration::ZERO;
        match state.store.due_routines(now).await {
            Ok(due) => {
                for routine in due {
                    if let Err(e) = fire(&state, &routine, RunTrigger::Schedule).await {
                        tracing::error!(routine = routine.id, error = %e, "routine failed to fire");
                        floor = ERROR_BACKOFF;
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "scheduler: cannot load due routines");
                floor = ERROR_BACKOFF;
            }
        }
        let wait = match state.store.next_routine_due().await {
            Ok(Some(next)) => {
                let secs = (next - OffsetDateTime::now_utc()).as_seconds_f64().max(0.0);
                Duration::from_secs_f64(secs).min(MAX_IDLE)
            }
            _ => MAX_IDLE,
        }
        .max(floor);
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = state.scheduler.notified() => {}
        }
    }
}

/// The host's IANA time zone, or UTC if it cannot be determined.
pub fn system_timezone() -> String {
    iana_time_zone::get_timezone()
        .ok()
        .filter(|tz| tz.parse::<Tz>().is_ok())
        .unwrap_or_else(|| "UTC".into())
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn weekday_morning_in_berlin_across_dst() {
        let s = Schedule::parse("0 9 * * 1-5", "Europe/Berlin").unwrap();
        // Friday 2026-03-27 10:00 UTC -> next is Monday 2026-03-30 09:00 CEST (07:00 UTC)
        let next = s.next_after(datetime!(2026-03-27 10:00 UTC)).unwrap();
        assert_eq!(next, datetime!(2026-03-30 07:00 UTC));
        // Before the switch: Friday 09:00 CET is 08:00 UTC
        let next = s.next_after(datetime!(2026-03-27 07:59 UTC)).unwrap();
        assert_eq!(next, datetime!(2026-03-27 08:00 UTC));
    }

    #[test]
    fn aliases_and_strictly_after() {
        let s = Schedule::parse("@hourly", "UTC").unwrap();
        let next = s.next_after(datetime!(2026-01-01 10:00 UTC)).unwrap();
        assert_eq!(next, datetime!(2026-01-01 11:00 UTC));
        let s = Schedule::parse("*/15 * * * *", "UTC").unwrap();
        let next = s.next_after(datetime!(2026-01-01 10:07:30 UTC)).unwrap();
        assert_eq!(next, datetime!(2026-01-01 10:15 UTC));
        assert_ne!(s.describe(), "");
    }

    #[test]
    fn rejects_bad_input() {
        for (expr, tz) in [
            ("", "UTC"),
            ("not cron", "UTC"),
            ("0 0 9 * * *", "UTC"), // seconds field
            ("61 * * * *", "UTC"),
            ("0 9 * * *", "Mars/Olympus"),
        ] {
            assert!(Schedule::parse(expr, tz).is_err(), "{expr} {tz}");
        }
    }
}
