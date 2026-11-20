//! Key/value memory with FTS5 full-text recall: per agent, plus one global
//! memory every agent shares.

use ferrobot_core::AgentId;
use rusqlite::{Row, params};
use serde::Serialize;
use time::OffsetDateTime;

use crate::util::{fmt_ts, now, parse_ts};
use crate::{Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct MemoryEntry {
    pub key: String,
    pub content: String,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub updated_at: OffsetDateTime,
}

fn from_row(row: &Row<'_>) -> Result<MemoryEntry, StoreError> {
    let updated: String = row.get(2)?;
    Ok(MemoryEntry {
        key: row.get(0)?,
        content: row.get(1)?,
        updated_at: parse_ts(&updated)?,
    })
}

/// Turns free text into a safe FTS5 query: every word becomes a quoted
/// prefix term, joined with OR. Prevents FTS syntax injection.
fn fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

impl Store {
    /// Creates or replaces the memory under `key`.
    pub async fn remember(
        &self,
        agent: AgentId,
        key: String,
        content: String,
    ) -> Result<(), StoreError> {
        self.with(move |c| {
            let ts = fmt_ts(now());
            c.execute(
                "INSERT INTO memories (agent_id, key, content, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT (agent_id, key) DO UPDATE SET content = excluded.content,
                                                          updated_at = excluded.updated_at",
                params![agent.to_string(), key, content, ts],
            )?;
            Ok(())
        })
        .await
    }

    /// Deletes the memory under `key`. Returns whether it existed.
    pub async fn forget(&self, agent: AgentId, key: String) -> Result<bool, StoreError> {
        self.with(move |c| {
            Ok(c.execute(
                "DELETE FROM memories WHERE agent_id = ?1 AND key = ?2",
                params![agent.to_string(), key],
            )? > 0)
        })
        .await
    }

    /// Full-text search over the agent's memories, best match first.
    /// An empty query returns the most recently updated entries.
    pub async fn recall(
        &self,
        agent: AgentId,
        query: String,
        limit: u32,
    ) -> Result<Vec<MemoryEntry>, StoreError> {
        self.with(move |c| {
            let id = agent.to_string();
            match fts_query(&query) {
                Some(q) => collect(
                    c,
                    "SELECT m.key, m.content, m.updated_at FROM memories_fts f
                     JOIN memories m ON m.id = f.rowid
                     WHERE memories_fts MATCH ?1 AND m.agent_id = ?2
                     ORDER BY bm25(memories_fts) LIMIT ?3",
                    params![q, id, limit],
                ),
                None => collect(
                    c,
                    "SELECT key, content, updated_at FROM memories WHERE agent_id = ?1
                     ORDER BY updated_at DESC LIMIT ?2",
                    params![id, limit],
                ),
            }
        })
        .await
    }

    /// [`Store::remember`] for the global memory.
    pub async fn remember_global(&self, key: String, content: String) -> Result<(), StoreError> {
        self.with(move |c| {
            let ts = fmt_ts(now());
            c.execute(
                "INSERT INTO global_memories (key, content, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT (key) DO UPDATE SET content = excluded.content,
                                                 updated_at = excluded.updated_at",
                params![key, content, ts],
            )?;
            Ok(())
        })
        .await
    }

    /// [`Store::forget`] for the global memory.
    pub async fn forget_global(&self, key: String) -> Result<bool, StoreError> {
        self.with(move |c| Ok(c.execute("DELETE FROM global_memories WHERE key = ?1", [key])? > 0))
            .await
    }

    /// [`Store::recall`] for the global memory.
    pub async fn recall_global(
        &self,
        query: String,
        limit: u32,
    ) -> Result<Vec<MemoryEntry>, StoreError> {
        self.with(move |c| match fts_query(&query) {
            Some(q) => collect(
                c,
                "SELECT m.key, m.content, m.updated_at FROM global_memories_fts f
                 JOIN global_memories m ON m.id = f.rowid
                 WHERE global_memories_fts MATCH ?1
                 ORDER BY bm25(global_memories_fts) LIMIT ?2",
                params![q, limit],
            ),
            None => collect(
                c,
                "SELECT key, content, updated_at FROM global_memories
                 ORDER BY updated_at DESC LIMIT ?1",
                params![limit],
            ),
        })
        .await
    }
}

fn collect(
    c: &rusqlite::Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<MemoryEntry>, StoreError> {
    let mut stmt = c.prepare(sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(from_row(row)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use ferrobot_core::AgentName;

    use super::*;
    use crate::NewAgent;

    async fn agent(store: &Store, name: &str) -> AgentId {
        store
            .create_agent(NewAgent {
                created_by: None,
                name: AgentName::parse(name).unwrap(),
                description: String::new(),
                instructions: String::new(),
            })
            .await
            .unwrap()
            .id
    }

    #[test]
    fn fts_query_quotes_terms() {
        assert_eq!(
            fts_query("deploy* OR\"x"),
            Some("\"deploy\"* OR \"OR\"* OR \"x\"*".into())
        );
        assert_eq!(fts_query("  ...  "), None);
    }

    #[tokio::test]
    async fn remember_recall_forget_isolated_per_agent() {
        let store = Store::open_in_memory().unwrap();
        let a = agent(&store, "alice").await;
        let b = agent(&store, "bob").await;
        store
            .remember(a, "deploy".into(), "Deploys run on Fridays".into())
            .await
            .unwrap();
        store
            .remember(a, "lunch".into(), "Pizza on Mondays".into())
            .await
            .unwrap();
        store
            .remember(b, "deploy".into(), "bob's secret".into())
            .await
            .unwrap();

        let hits = store.recall(a, "friday deploys".into(), 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "deploy");

        // upsert updates the FTS index too
        store
            .remember(a, "deploy".into(), "Deploys run on Tuesdays".into())
            .await
            .unwrap();
        assert_eq!(store.recall(a, "friday".into(), 5).await.unwrap(), []);
        assert_eq!(store.recall(a, "tuesday".into(), 5).await.unwrap().len(), 1);

        assert_eq!(store.recall(a, String::new(), 10).await.unwrap().len(), 2);
        assert!(store.forget(a, "lunch".into()).await.unwrap());
        assert!(!store.forget(a, "lunch".into()).await.unwrap());
        assert_eq!(store.recall(a, "pizza".into(), 5).await.unwrap(), []);
    }

    #[tokio::test]
    async fn global_memory_is_separate_from_agents() {
        let store = Store::open_in_memory().unwrap();
        let a = agent(&store, "alice").await;
        store
            .remember(a, "home".into(), "alice's own note".into())
            .await
            .unwrap();
        store
            .remember_global("home".into(), "Tim lives in Berlin".into())
            .await
            .unwrap();
        store
            .remember_global("home".into(), "Tim lives in Hamburg".into())
            .await
            .unwrap();
        let hits = store.recall_global("hamburg".into(), 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content, "Tim lives in Hamburg");
        assert_eq!(store.recall_global("berlin".into(), 5).await.unwrap(), []);
        assert_eq!(store.recall(a, "hamburg".into(), 5).await.unwrap(), []);
        assert_eq!(
            store.recall_global(String::new(), 5).await.unwrap().len(),
            1
        );
        assert!(store.forget_global("home".into()).await.unwrap());
        assert!(!store.forget_global("home".into()).await.unwrap());
        assert_eq!(store.recall(a, String::new(), 5).await.unwrap().len(), 1);
    }
}
