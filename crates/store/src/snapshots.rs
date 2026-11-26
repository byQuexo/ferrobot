//! Records of sandbox snapshots (the backend owns the actual data).

use ferrobot_core::AgentId;
use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;
use time::OffsetDateTime;

use crate::util::{fmt_ts, now, parse_ts};
use crate::{Store, StoreError};

/// A snapshot of an agent's sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SnapshotRecord {
    pub id: i64,
    pub agent_id: AgentId,
    /// Backend that created it, e.g. `docker`.
    pub backend: String,
    /// Backend-specific reference (e.g. an image tag).
    pub snapshot_ref: String,
    pub label: String,
    /// What the snapshot covers.
    pub scope: String,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

const COLUMNS: &str = "id, agent_id, backend, snapshot_ref, label, scope, created_at";

fn from_row(row: &Row<'_>) -> Result<SnapshotRecord, StoreError> {
    let agent: String = row.get(1)?;
    let created: String = row.get(6)?;
    Ok(SnapshotRecord {
        id: row.get(0)?,
        agent_id: agent.parse()?,
        backend: row.get(2)?,
        snapshot_ref: row.get(3)?,
        label: row.get(4)?,
        scope: row.get(5)?,
        created_at: parse_ts(&created)?,
    })
}

impl Store {
    pub async fn add_snapshot(
        &self,
        agent: AgentId,
        backend: String,
        snapshot_ref: String,
        label: String,
        scope: String,
    ) -> Result<SnapshotRecord, StoreError> {
        self.with(move |c| {
            c.execute(
                "INSERT INTO snapshots (agent_id, backend, snapshot_ref, label, scope, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    agent.to_string(),
                    backend,
                    snapshot_ref,
                    label,
                    scope,
                    fmt_ts(now())
                ],
            )?;
            let id = c.last_insert_rowid();
            c.query_row(
                &format!("SELECT {COLUMNS} FROM snapshots WHERE id = ?1"),
                [id],
                |r| Ok(from_row(r)),
            )?
        })
        .await
    }

    /// Snapshots of an agent, newest first.
    pub async fn list_snapshots(&self, agent: AgentId) -> Result<Vec<SnapshotRecord>, StoreError> {
        self.with(move |c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUMNS} FROM snapshots WHERE agent_id = ?1 ORDER BY id DESC"
            ))?;
            let rows = stmt.query_map([agent.to_string()], |r| Ok(from_row(r)))?;
            rows.map(|r| r?).collect()
        })
        .await
    }

    pub async fn get_snapshot(&self, id: i64) -> Result<SnapshotRecord, StoreError> {
        self.with(move |c| {
            c.query_row(
                &format!("SELECT {COLUMNS} FROM snapshots WHERE id = ?1"),
                [id],
                |r| Ok(from_row(r)),
            )
            .optional()?
            .ok_or_else(|| StoreError::not_found("snapshot", id))?
        })
        .await
    }

    pub async fn delete_snapshot(&self, id: i64) -> Result<(), StoreError> {
        self.with(move |c| {
            c.execute("DELETE FROM snapshots WHERE id = ?1", [id])?;
            Ok(())
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
    async fn add_list_delete() {
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
        let s1 = store
            .add_snapshot(
                a.id,
                "docker".into(),
                "img:1".into(),
                "one".into(),
                "all".into(),
            )
            .await
            .unwrap();
        let s2 = store
            .add_snapshot(
                a.id,
                "docker".into(),
                "img:2".into(),
                "two".into(),
                "all".into(),
            )
            .await
            .unwrap();
        let ids: Vec<i64> = store
            .list_snapshots(a.id)
            .await
            .unwrap()
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, [s2.id, s1.id]);
        assert_eq!(
            store.get_snapshot(s1.id).await.unwrap().snapshot_ref,
            "img:1"
        );
        store.delete_snapshot(s1.id).await.unwrap();
        assert!(store.get_snapshot(s1.id).await.is_err());
    }
}
