use ferrobot_core::{Agent, AgentId, AgentName, AgentStatus};
use rusqlite::{OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

use crate::util::{fmt_ts, is_unique_violation, now, parse_ts};
use crate::{Store, StoreError};

/// Input for registering an agent.
#[derive(Debug, Clone)]
pub struct NewAgent {
    pub name: AgentName,
    pub instructions: String,
}

/// SHA-256 of a runner token. Only hashes are stored.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

const COLUMNS: &str =
    "id, name, instructions, status, backend, sandbox_id, session_id, created_at";

fn from_row(row: &Row<'_>) -> Result<Agent, StoreError> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let status: String = row.get(3)?;
    let created: String = row.get(7)?;
    Ok(Agent {
        id: id.parse()?,
        name: AgentName::parse(name)?,
        instructions: row.get(2)?,
        status: status.parse()?,
        backend: row.get(4)?,
        sandbox_id: row.get(5)?,
        session_id: row.get(6)?,
        created_at: parse_ts(&created)?,
    })
}

fn not_found(key: impl ToString) -> StoreError {
    StoreError::NotFound {
        entity: "agent",
        key: key.to_string(),
    }
}

impl Store {
    pub async fn create_agent(&self, new: NewAgent) -> Result<Agent, StoreError> {
        let agent = Agent {
            id: AgentId::new(),
            name: new.name,
            instructions: new.instructions,
            status: AgentStatus::Created,
            backend: None,
            sandbox_id: None,
            session_id: None,
            created_at: now(),
        };
        let row = agent.clone();
        self.with(move |c| {
            c.execute(
                "INSERT INTO agents (id, name, instructions, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    row.id.to_string(),
                    row.name.as_str(),
                    row.instructions,
                    row.status.as_str(),
                    fmt_ts(row.created_at)
                ],
            )
            .map_err(|e| {
                if is_unique_violation(&e) {
                    StoreError::Conflict {
                        entity: "agent",
                        key: row.name.to_string(),
                    }
                } else {
                    e.into()
                }
            })?;
            Ok(())
        })
        .await?;
        Ok(agent)
    }

    pub async fn list_agents(&self) -> Result<Vec<Agent>, StoreError> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!("SELECT {COLUMNS} FROM agents ORDER BY name"))?;
            let mut rows = stmt.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(from_row(row)?);
            }
            Ok(out)
        })
        .await
    }

    pub async fn get_agent(&self, id: AgentId) -> Result<Agent, StoreError> {
        self.with(move |c| {
            c.query_row(
                &format!("SELECT {COLUMNS} FROM agents WHERE id = ?1"),
                [id.to_string()],
                |row| Ok(from_row(row)),
            )
            .optional()?
            .ok_or_else(|| not_found(id))?
        })
        .await
    }

    pub async fn get_agent_by_name(&self, name: AgentName) -> Result<Agent, StoreError> {
        self.with(move |c| {
            c.query_row(
                &format!("SELECT {COLUMNS} FROM agents WHERE name = ?1"),
                [name.as_str()],
                |row| Ok(from_row(row)),
            )
            .optional()?
            .ok_or_else(|| not_found(&name))?
        })
        .await
    }

    /// Resolves a runner token to its agent. Returns `None` for unknown tokens.
    pub async fn agent_by_token(&self, token: &str) -> Result<Option<Agent>, StoreError> {
        let hash = hash_token(token);
        self.with(move |c| {
            c.query_row(
                &format!("SELECT {COLUMNS} FROM agents WHERE token_hash = ?1"),
                [hash],
                |row| Ok(from_row(row)),
            )
            .optional()?
            .transpose()
        })
        .await
    }

    /// Stores the hash of a freshly issued runner token (rotates the old one).
    pub async fn set_agent_token(&self, id: AgentId, token: &str) -> Result<(), StoreError> {
        let hash = hash_token(token);
        self.update_agent(id, move |c, id| {
            c.execute(
                "UPDATE agents SET token_hash = ?2 WHERE id = ?1",
                params![id, hash],
            )
        })
        .await
    }

    pub async fn set_agent_sandbox(
        &self,
        id: AgentId,
        backend: String,
        sandbox_id: String,
    ) -> Result<(), StoreError> {
        self.update_agent(id, move |c, id| {
            c.execute(
                "UPDATE agents SET backend = ?2, sandbox_id = ?3 WHERE id = ?1",
                params![id, backend, sandbox_id],
            )
        })
        .await
    }

    pub async fn set_agent_status(&self, id: AgentId, status: AgentStatus) -> Result<(), StoreError> {
        self.update_agent(id, move |c, id| {
            c.execute(
                "UPDATE agents SET status = ?2 WHERE id = ?1",
                params![id, status.as_str()],
            )
        })
        .await
    }

    pub async fn set_agent_session(&self, id: AgentId, session_id: String) -> Result<(), StoreError> {
        self.update_agent(id, move |c, id| {
            c.execute(
                "UPDATE agents SET session_id = ?2 WHERE id = ?1",
                params![id, session_id],
            )
        })
        .await
    }

    pub async fn delete_agent(&self, id: AgentId) -> Result<(), StoreError> {
        self.update_agent(id, |c, id| c.execute("DELETE FROM agents WHERE id = ?1", [id]))
            .await
    }

    async fn update_agent<F>(&self, id: AgentId, f: F) -> Result<(), StoreError>
    where
        F: FnOnce(&rusqlite::Connection, String) -> rusqlite::Result<usize> + Send + 'static,
    {
        self.with(move |c| match f(c, id.to_string())? {
            0 => Err(not_found(id)),
            _ => Ok(()),
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(name: &str) -> NewAgent {
        NewAgent {
            name: AgentName::parse(name).unwrap(),
            instructions: "test".into(),
        }
    }

    #[tokio::test]
    async fn create_get_list_delete() {
        let store = Store::open_in_memory().unwrap();
        let a = store.create_agent(new("alice")).await.unwrap();
        store.create_agent(new("bob")).await.unwrap();
        assert_eq!(store.get_agent(a.id).await.unwrap(), a);
        let names: Vec<_> = store
            .list_agents()
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.name.to_string())
            .collect();
        assert_eq!(names, ["alice", "bob"]);
        store.delete_agent(a.id).await.unwrap();
        assert!(matches!(
            store.get_agent(a.id).await,
            Err(StoreError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn duplicate_name_conflicts() {
        let store = Store::open_in_memory().unwrap();
        store.create_agent(new("alice")).await.unwrap();
        assert!(matches!(
            store.create_agent(new("alice")).await,
            Err(StoreError::Conflict { .. })
        ));
    }

    #[tokio::test]
    async fn token_lookup_uses_hash_and_rotates() {
        let store = Store::open_in_memory().unwrap();
        let a = store.create_agent(new("alice")).await.unwrap();
        store.set_agent_token(a.id, "secret-1").await.unwrap();
        assert_eq!(store.agent_by_token("secret-1").await.unwrap().unwrap().id, a.id);
        store.set_agent_token(a.id, "secret-2").await.unwrap();
        assert!(store.agent_by_token("secret-1").await.unwrap().is_none());
        assert!(store.agent_by_token("secret-2").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn status_and_session_updates() {
        let store = Store::open_in_memory().unwrap();
        let a = store.create_agent(new("alice")).await.unwrap();
        store.set_agent_status(a.id, AgentStatus::Running).await.unwrap();
        store.set_agent_session(a.id, "s-1".into()).await.unwrap();
        store
            .set_agent_sandbox(a.id, "docker".into(), "c1".into())
            .await
            .unwrap();
        let got = store.get_agent(a.id).await.unwrap();
        assert_eq!(got.status, AgentStatus::Running);
        assert_eq!(got.session_id.as_deref(), Some("s-1"));
        assert_eq!(got.sandbox_id.as_deref(), Some("c1"));
    }
}
