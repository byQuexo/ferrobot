//! Append-only log of turn events per agent (what the UI renders).

use ferrobot_core::AgentId;
use rusqlite::params;
use serde::Serialize;
use time::OffsetDateTime;

use crate::util::{fmt_ts, now, parse_ts};
use crate::{Store, StoreError};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct StoredEvent {
    pub id: i64,
    pub agent_id: AgentId,
    pub turn_id: Option<String>,
    pub kind: String,
    /// The event body; for runner events this is a `TurnEvent`.
    #[cfg_attr(feature = "schema", schema(value_type = Object))]
    pub payload: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

impl Store {
    pub async fn append_event(
        &self,
        agent: AgentId,
        turn_id: Option<String>,
        kind: String,
        payload: serde_json::Value,
    ) -> Result<StoredEvent, StoreError> {
        self.with(move |c| {
            let created_at = now();
            c.execute(
                "INSERT INTO events (agent_id, turn_id, kind, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    agent.to_string(),
                    turn_id,
                    kind,
                    payload.to_string(),
                    fmt_ts(created_at)
                ],
            )?;
            Ok(StoredEvent {
                id: c.last_insert_rowid(),
                agent_id: agent,
                turn_id,
                kind,
                payload,
                created_at,
            })
        })
        .await
    }

    /// One event of `agent`.
    pub async fn get_event(&self, agent: AgentId, id: i64) -> Result<StoredEvent, StoreError> {
        self.with(move |c| {
            let row = c.query_row(
                "SELECT turn_id, kind, payload, created_at FROM events
                 WHERE agent_id = ?1 AND id = ?2",
                params![agent.to_string(), id],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            );
            let (turn_id, kind, payload, created) = match row {
                Ok(row) => row,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return Err(StoreError::NotFound {
                        entity: "event",
                        key: id.to_string(),
                    });
                }
                Err(e) => return Err(e.into()),
            };
            Ok(StoredEvent {
                id,
                agent_id: agent,
                turn_id,
                kind,
                payload: serde_json::from_str(&payload).map_err(StoreError::corrupt)?,
                created_at: parse_ts(&created)?,
            })
        })
        .await
    }

    /// Whether a `question_answered` event exists for question event `question`.
    pub async fn question_answered(
        &self,
        agent: AgentId,
        question: i64,
    ) -> Result<bool, StoreError> {
        self.with(move |c| {
            Ok(c.query_row(
                "SELECT EXISTS (SELECT 1 FROM events WHERE agent_id = ?1
                 AND kind = 'question_answered' AND json_extract(payload, '$.question') = ?2)",
                params![agent.to_string(), question],
                |r| r.get(0),
            )?)
        })
        .await
    }

    /// Whether `agent` has any event of `kind`.
    pub async fn has_event(&self, agent: AgentId, kind: &'static str) -> Result<bool, StoreError> {
        self.with(move |c| {
            Ok(c.query_row(
                "SELECT EXISTS (SELECT 1 FROM events WHERE agent_id = ?1 AND kind = ?2)",
                params![agent.to_string(), kind],
                |r| r.get(0),
            )?)
        })
        .await
    }

    /// Events of `agent` with id greater than `after` (oldest first).
    pub async fn events_after(
        &self,
        agent: AgentId,
        after: i64,
        limit: u32,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        self.with(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, turn_id, kind, payload, created_at FROM events
                 WHERE agent_id = ?1 AND id > ?2 ORDER BY id LIMIT ?3",
            )?;
            let mut rows = stmt.query(params![agent.to_string(), after, limit])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let payload: String = row.get(3)?;
                let created: String = row.get(4)?;
                out.push(StoredEvent {
                    id: row.get(0)?,
                    agent_id: agent,
                    turn_id: row.get(1)?,
                    kind: row.get(2)?,
                    payload: serde_json::from_str(&payload).map_err(StoreError::corrupt)?,
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

    use super::*;
    use crate::NewAgent;

    #[tokio::test]
    async fn append_and_page() {
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
        let e1 = store
            .append_event(
                a.id,
                Some("t".into()),
                "assistant_text".into(),
                serde_json::json!({"text": "hi"}),
            )
            .await
            .unwrap();
        store
            .append_event(a.id, None, "status".into(), serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(store.events_after(a.id, 0, 10).await.unwrap().len(), 2);
        let rest = store.events_after(a.id, e1.id, 10).await.unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].kind, "status");
    }
}
