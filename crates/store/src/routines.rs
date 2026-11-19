//! Routines: prompts sent to an agent on a cron schedule, with run history.
//!
//! The store keeps routines and their next due time; computing that time from
//! the cron expression is the scheduler's job (in the daemon), so this crate
//! stays free of calendar logic.

use ferrobot_core::{AgentId, MailMessage, MailStatus, Sender};
use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;
use time::OffsetDateTime;

use crate::mailbox::insert_message;
use crate::util::{fmt_ts, is_unique_violation, now, parse_ts};
use crate::{Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct Routine {
    pub id: i64,
    pub agent_id: AgentId,
    pub name: String,
    /// Cron expression (5 fields, minute granularity) or an alias like `@hourly`.
    pub schedule: String,
    /// IANA time zone the schedule is evaluated in, e.g. `Europe/Berlin`.
    pub timezone: String,
    /// Message sent to the agent on every run.
    pub prompt: String,
    pub enabled: bool,
    /// Next scheduled run; null while disabled.
    #[serde(with = "time::serde::rfc3339::option")]
    #[cfg_attr(feature = "schema", schema(value_type = Option<String>, format = DateTime))]
    pub next_run_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[cfg_attr(feature = "schema", schema(value_type = Option<String>, format = DateTime))]
    pub last_run_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

/// Input for creating a routine.
#[derive(Debug, Clone)]
pub struct NewRoutine {
    pub name: String,
    pub schedule: String,
    pub timezone: String,
    pub prompt: String,
    pub enabled: bool,
    pub next_run_at: Option<OffsetDateTime>,
}

/// What started a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RunTrigger {
    Schedule,
    Manual,
}

impl RunTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Manual => "manual",
        }
    }
}

/// One execution of a routine: the message it queued and how that went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct RoutineRun {
    pub id: i64,
    pub routine_id: i64,
    pub message_id: Option<i64>,
    pub trigger: RunTrigger,
    /// Delivery status of the message; null if the message was deleted.
    pub status: Option<MailStatus>,
    /// Last delivery error, if any.
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

const COLUMNS: &str = "id, agent_id, name, schedule, timezone, prompt, enabled, next_run_at, \
                       last_run_at, created_at";

fn opt_ts(raw: Option<&str>) -> Result<Option<OffsetDateTime>, StoreError> {
    raw.map(parse_ts).transpose()
}

fn from_row(row: &Row<'_>) -> Result<Routine, StoreError> {
    let agent: String = row.get(1)?;
    let created: String = row.get(9)?;
    Ok(Routine {
        id: row.get(0)?,
        agent_id: agent.parse()?,
        name: row.get(2)?,
        schedule: row.get(3)?,
        timezone: row.get(4)?,
        prompt: row.get(5)?,
        enabled: row.get(6)?,
        next_run_at: opt_ts(row.get::<_, Option<String>>(7)?.as_deref())?,
        last_run_at: opt_ts(row.get::<_, Option<String>>(8)?.as_deref())?,
        created_at: parse_ts(&created)?,
    })
}

fn not_found(id: i64) -> StoreError {
    StoreError::NotFound {
        entity: "routine",
        key: id.to_string(),
    }
}

fn get(c: &rusqlite::Connection, id: i64) -> Result<Routine, StoreError> {
    c.query_row(
        &format!("SELECT {COLUMNS} FROM routines WHERE id = ?1"),
        [id],
        |row| Ok(from_row(row)),
    )
    .optional()?
    .ok_or_else(|| not_found(id))?
}

fn conflict(name: &str) -> impl FnOnce(rusqlite::Error) -> StoreError + '_ {
    move |e| {
        if is_unique_violation(&e) {
            StoreError::Conflict {
                entity: "routine",
                key: name.to_owned(),
            }
        } else {
            e.into()
        }
    }
}

impl Store {
    pub async fn create_routine(
        &self,
        agent: AgentId,
        new: NewRoutine,
    ) -> Result<Routine, StoreError> {
        self.with(move |c| {
            let ts = fmt_ts(now());
            c.execute(
                "INSERT INTO routines (agent_id, name, schedule, timezone, prompt, enabled,
                                       next_run_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    agent.to_string(),
                    new.name,
                    new.schedule,
                    new.timezone,
                    new.prompt,
                    new.enabled,
                    new.next_run_at.map(fmt_ts),
                    ts
                ],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _)
                    if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY =>
                {
                    StoreError::NotFound {
                        entity: "agent",
                        key: agent.to_string(),
                    }
                }
                other => conflict(&new.name)(other),
            })?;
            get(c, c.last_insert_rowid())
        })
        .await
    }

    pub async fn get_routine(&self, id: i64) -> Result<Routine, StoreError> {
        self.with(move |c| get(c, id)).await
    }

    /// Replaces all editable fields of a routine.
    pub async fn update_routine(&self, routine: Routine) -> Result<Routine, StoreError> {
        self.with(move |c| {
            let changed = c
                .execute(
                    "UPDATE routines SET name = ?2, schedule = ?3, timezone = ?4, prompt = ?5,
                            enabled = ?6, next_run_at = ?7, updated_at = ?8
                     WHERE id = ?1",
                    params![
                        routine.id,
                        routine.name,
                        routine.schedule,
                        routine.timezone,
                        routine.prompt,
                        routine.enabled,
                        routine.next_run_at.map(fmt_ts),
                        fmt_ts(now())
                    ],
                )
                .map_err(conflict(&routine.name))?;
            if changed == 0 {
                return Err(not_found(routine.id));
            }
            get(c, routine.id)
        })
        .await
    }

    pub async fn delete_routine(&self, id: i64) -> Result<Routine, StoreError> {
        self.with(move |c| {
            let routine = get(c, id)?;
            c.execute("DELETE FROM routines WHERE id = ?1", [id])?;
            Ok(routine)
        })
        .await
    }

    pub async fn list_routines(&self, agent: AgentId) -> Result<Vec<Routine>, StoreError> {
        self.query_routines("WHERE agent_id = ?1 ORDER BY name", vec![agent.to_string()])
            .await
    }

    /// Enabled routines whose next run is at or before `at`.
    pub async fn due_routines(&self, at: OffsetDateTime) -> Result<Vec<Routine>, StoreError> {
        self.query_routines(
            "WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1 ORDER BY next_run_at",
            vec![fmt_ts(at)],
        )
        .await
    }

    /// Earliest upcoming run of any enabled routine.
    pub async fn next_routine_due(&self) -> Result<Option<OffsetDateTime>, StoreError> {
        self.with(|c| {
            let raw: Option<String> = c.query_row(
                "SELECT MIN(next_run_at) FROM routines WHERE enabled = 1",
                [],
                |r| r.get(0),
            )?;
            opt_ts(raw.as_deref())
        })
        .await
    }

    async fn query_routines(
        &self,
        clause: &'static str,
        args: Vec<String>,
    ) -> Result<Vec<Routine>, StoreError> {
        self.with(move |c| {
            let mut stmt = c.prepare(&format!("SELECT {COLUMNS} FROM routines {clause}"))?;
            let mut rows = stmt.query(rusqlite::params_from_iter(args))?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(from_row(row)?);
            }
            Ok(out)
        })
        .await
    }

    /// Fires a routine atomically: queues its prompt as a message from
    /// `routine:<id>`, records the run and stores the next due time.
    pub async fn fire_routine(
        &self,
        id: i64,
        trigger: RunTrigger,
        next_run_at: Option<OffsetDateTime>,
    ) -> Result<(Routine, MailMessage), StoreError> {
        self.with(move |c| {
            let tx = c.transaction()?;
            let routine = get(&tx, id)?;
            let msg = insert_message(
                &tx,
                Sender::Routine(id),
                routine.agent_id,
                &routine.prompt,
                None,
                None,
            )?;
            let ts = fmt_ts(now());
            tx.execute(
                "INSERT INTO routine_runs (routine_id, message_id, trigger, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, msg.id, trigger.as_str(), ts],
            )?;
            // A manual run does not move the schedule.
            if trigger == RunTrigger::Schedule {
                tx.execute(
                    "UPDATE routines SET last_run_at = ?2, next_run_at = ?3 WHERE id = ?1",
                    params![id, ts, next_run_at.map(fmt_ts)],
                )?;
            } else {
                tx.execute(
                    "UPDATE routines SET last_run_at = ?2 WHERE id = ?1",
                    params![id, ts],
                )?;
            }
            let routine = get(&tx, id)?;
            tx.commit()?;
            Ok((routine, msg))
        })
        .await
    }

    /// Most recent runs of a routine, newest first.
    pub async fn routine_runs(
        &self,
        routine: i64,
        limit: u32,
    ) -> Result<Vec<RoutineRun>, StoreError> {
        self.with(move |c| {
            let mut stmt = c.prepare(
                "SELECT r.id, r.message_id, r.trigger, m.status, m.last_error, r.created_at
                 FROM routine_runs r LEFT JOIN messages m ON m.id = r.message_id
                 WHERE r.routine_id = ?1 ORDER BY r.id DESC LIMIT ?2",
            )?;
            let mut rows = stmt.query(params![routine, limit])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let trigger: String = row.get(2)?;
                let status: Option<String> = row.get(3)?;
                let created: String = row.get(5)?;
                out.push(RoutineRun {
                    id: row.get(0)?,
                    routine_id: routine,
                    message_id: row.get(1)?,
                    trigger: if trigger == "manual" {
                        RunTrigger::Manual
                    } else {
                        RunTrigger::Schedule
                    },
                    status: status.map(|s| s.parse()).transpose()?,
                    error: row.get(4)?,
                    created_at: parse_ts(&created)?,
                });
            }
            Ok(out)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use ferrobot_core::AgentName;
    use time::Duration;

    use super::*;
    use crate::NewAgent;

    fn new_routine(name: &str, next: Option<OffsetDateTime>) -> NewRoutine {
        NewRoutine {
            name: name.into(),
            schedule: "0 9 * * *".into(),
            timezone: "Europe/Berlin".into(),
            prompt: "summarize".into(),
            enabled: next.is_some(),
            next_run_at: next,
        }
    }

    #[tokio::test]
    async fn crud_due_fire_and_history() {
        let store = Store::open_in_memory().unwrap();
        let a = store
            .create_agent(NewAgent {
                created_by: None,
                name: AgentName::parse("alice").unwrap(),
                description: String::new(),
                instructions: String::new(),
            })
            .await
            .unwrap();
        let t0 = now();
        let r = store
            .create_routine(a.id, new_routine("digest", Some(t0 - Duration::minutes(1))))
            .await
            .unwrap();
        let later = store
            .create_routine(a.id, new_routine("later", Some(t0 + Duration::hours(1))))
            .await
            .unwrap();
        store
            .create_routine(a.id, new_routine("off", None))
            .await
            .unwrap();
        assert!(matches!(
            store
                .create_routine(a.id, new_routine("digest", None))
                .await,
            Err(StoreError::Conflict { .. })
        ));
        assert_eq!(store.list_routines(a.id).await.unwrap().len(), 3);

        let due = store.due_routines(t0).await.unwrap();
        assert_eq!(due.iter().map(|r| r.id).collect::<Vec<_>>(), [r.id]);

        let next = t0 + Duration::days(1);
        let (fired, msg) = store
            .fire_routine(r.id, RunTrigger::Schedule, Some(next))
            .await
            .unwrap();
        assert_eq!(msg.sender, Sender::Routine(r.id));
        assert_eq!(msg.body, "summarize");
        assert!(fired.last_run_at.is_some());
        assert_eq!(store.due_routines(t0).await.unwrap(), vec![]);
        assert_eq!(
            store.next_routine_due().await.unwrap().map(fmt_ts),
            later.next_run_at.map(fmt_ts)
        );

        store
            .fire_routine(r.id, RunTrigger::Manual, None)
            .await
            .unwrap();
        let runs = store.routine_runs(r.id, 10).await.unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].trigger, RunTrigger::Manual);
        assert_eq!(runs[1].status, Some(MailStatus::Pending));
        // manual run kept the schedule
        let again = store.get_routine(r.id).await.unwrap();
        assert_eq!(again.next_run_at.map(fmt_ts), Some(fmt_ts(next)));

        let mut edited = again.clone();
        edited.enabled = false;
        edited.next_run_at = None;
        let edited = store.update_routine(edited).await.unwrap();
        assert!(!edited.enabled);
        store.delete_routine(r.id).await.unwrap();
        assert!(matches!(
            store.get_routine(r.id).await,
            Err(StoreError::NotFound { .. })
        ));
    }
}
