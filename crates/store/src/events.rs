//! Append-only log of turn events per agent (what the UI renders).

use ferrobot_core::AgentId;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use time::OffsetDateTime;

use crate::util::{fmt_ts, now, parse_ts};
use crate::{Store, StoreError};

/// One recorded event in an agent's chat.
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

    /// The newest unanswered approval card in `agent`'s chat whose approval is of `kind`.
    pub async fn open_approval(
        &self,
        agent: AgentId,
        kind: &'static str,
    ) -> Result<Option<i64>, StoreError> {
        self.with(move |c| {
            Ok(c.query_row(
                "SELECT q.id FROM events q WHERE q.agent_id = ?1 AND q.kind = 'question'
                 AND json_extract(q.payload, '$.approval.kind') = ?2
                 AND NOT EXISTS (SELECT 1 FROM events a WHERE a.agent_id = ?1
                     AND a.kind = 'question_answered' AND json_extract(a.payload, '$.question') = q.id)
                 ORDER BY q.id DESC LIMIT 1",
                params![agent.to_string(), kind],
                |r| r.get(0),
            )
            .optional()?)
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

    /// Whether the operator started a new session (a `new_session` event) after `agent`'s last
    /// turn began; a `message_in` steered into a running turn does not count as a turn.
    pub async fn new_session_pending(&self, agent: AgentId) -> Result<bool, StoreError> {
        self.with(move |c| {
            let kind: Option<String> = c
                .query_row(
                    "SELECT kind FROM events WHERE agent_id = ?1 AND (kind = 'new_session'
                         OR (kind = 'message_in' AND json_extract(payload, '$.steered') IS NULL))
                     ORDER BY id DESC LIMIT 1",
                    [agent.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(kind.as_deref() == Some("new_session"))
        })
        .await
    }

    /// Renames since `agent`'s last turn began, oldest first: the renamed agent and its old
    /// name. Empty for an agent that has had no turn, so never knew the old names.
    pub async fn renames_since_last_turn(
        &self,
        agent: AgentId,
    ) -> Result<Vec<(AgentId, String)>, StoreError> {
        self.with(move |c| {
            let mut stmt = c.prepare(
                "SELECT agent_id, json_extract(payload, '$.from') FROM events
                 WHERE kind = 'renamed' AND id > (
                     SELECT MAX(id) FROM events WHERE agent_id = ?1 AND kind = 'message_in')
                 ORDER BY id",
            )?;
            let rows = stmt.query_map([agent.to_string()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (id, from) = row?;
                if let (Ok(id), Some(from)) = (id.parse(), from) {
                    out.push((id, from));
                }
            }
            Ok(out)
        })
        .await
    }

    /// The newest `file` event `agent` shared from `path`, with when it was shared.
    pub async fn latest_file(
        &self,
        agent: AgentId,
        path: String,
    ) -> Result<Option<(i64, OffsetDateTime)>, StoreError> {
        self.with(move |c| {
            let row: Option<(i64, String)> = c
                .query_row(
                    "SELECT id, created_at FROM events
                     WHERE agent_id = ?1 AND kind = 'file' AND json_extract(payload, '$.path') = ?2
                     ORDER BY id DESC LIMIT 1",
                    params![agent.to_string(), path],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            row.map(|(id, at)| Ok((id, parse_ts(&at)?))).transpose()
        })
        .await
    }

    /// The events of one of `agent`'s turns, oldest first.
    pub async fn turn_events(
        &self,
        agent: AgentId,
        turn: String,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        self.with(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, kind, payload, created_at FROM events
                 WHERE agent_id = ?1 AND turn_id = ?2 ORDER BY id",
            )?;
            let mut rows = stmt.query(params![agent.to_string(), turn])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let payload: String = row.get(2)?;
                let created: String = row.get(3)?;
                out.push(StoredEvent {
                    id: row.get(0)?,
                    agent_id: agent,
                    turn_id: Some(turn.clone()),
                    kind: row.get(1)?,
                    payload: serde_json::from_str(&payload).map_err(StoreError::corrupt)?,
                    created_at: parse_ts(&created)?,
                });
            }
            Ok(out)
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

    #[tokio::test]
    async fn new_session_waits_for_the_next_turn() {
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
        let add = |kind: &str, payload: serde_json::Value| {
            store.append_event(a.id, None, kind.into(), payload)
        };
        assert!(!store.new_session_pending(a.id).await.unwrap());
        add("message_in", serde_json::json!({})).await.unwrap();
        add("new_session", serde_json::json!({})).await.unwrap();
        add("message_in", serde_json::json!({"steered": true}))
            .await
            .unwrap();
        assert!(store.new_session_pending(a.id).await.unwrap());
        add("message_in", serde_json::json!({})).await.unwrap();
        assert!(!store.new_session_pending(a.id).await.unwrap());
    }
}
