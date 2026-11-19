//! Routines (scheduled prompts). Phase 0 ships the schema and CRUD only;
//! the scheduler that turns due routines into mailbox messages is phase 1.

use ferrobot_core::AgentId;
use rusqlite::params;
use serde::Serialize;
use time::{Duration, OffsetDateTime};

use crate::util::{fmt_ts, now, parse_ts};
use crate::{Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Routine {
    pub id: i64,
    pub agent_id: AgentId,
    pub name: String,
    pub interval_s: u32,
    pub prompt: String,
    pub enabled: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub next_due_at: OffsetDateTime,
}

impl Store {
    pub async fn create_routine(
        &self,
        agent: AgentId,
        name: String,
        interval_s: u32,
        prompt: String,
    ) -> Result<Routine, StoreError> {
        self.with(move |c| {
            let created = now();
            let next_due_at = created + Duration::seconds(i64::from(interval_s));
            c.execute(
                "INSERT INTO routines (agent_id, name, interval_s, prompt, next_due_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    agent.to_string(),
                    name,
                    interval_s,
                    prompt,
                    fmt_ts(next_due_at),
                    fmt_ts(created)
                ],
            )?;
            Ok(Routine {
                id: c.last_insert_rowid(),
                agent_id: agent,
                name,
                interval_s,
                prompt,
                enabled: true,
                next_due_at,
            })
        })
        .await
    }

    pub async fn list_routines(&self, agent: AgentId) -> Result<Vec<Routine>, StoreError> {
        self.with(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, name, interval_s, prompt, enabled, next_due_at FROM routines
                 WHERE agent_id = ?1 ORDER BY name",
            )?;
            let mut rows = stmt.query([agent.to_string()])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let due: String = row.get(5)?;
                out.push(Routine {
                    id: row.get(0)?,
                    agent_id: agent,
                    name: row.get(1)?,
                    interval_s: row.get(2)?,
                    prompt: row.get(3)?,
                    enabled: row.get(4)?,
                    next_due_at: parse_ts(&due)?,
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

    use super::*;
    use crate::NewAgent;

    #[tokio::test]
    async fn create_and_list() {
        let store = Store::open_in_memory().unwrap();
        let a = store
            .create_agent(NewAgent {
                name: AgentName::parse("alice").unwrap(),
                instructions: String::new(),
            })
            .await
            .unwrap();
        store.create_routine(a.id, "digest".into(), 3600, "summarize".into()).await.unwrap();
        let list = store.list_routines(a.id).await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].enabled);
        // CHECK constraint: at least one minute
        assert!(store.create_routine(a.id, "spam".into(), 5, "x".into()).await.is_err());
    }
}
