//! Connector configuration and per-agent assignment. Tokens are not stored here.

use ferrobot_core::{AgentId, Connector};
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

use crate::util::{fmt_ts, is_unique_violation, parse_ts};
use crate::{Store, StoreError};

const COLUMNS: &str = "id, name, url, created_at, command, token_env";

fn from_row(row: &Row<'_>) -> Result<Connector, StoreError> {
    let id: String = row.get(0)?;
    let created: String = row.get(3)?;
    let command: Option<String> = row.get(4)?;
    Ok(Connector {
        id: Uuid::parse_str(&id).map_err(StoreError::corrupt)?,
        name: row.get(1)?,
        url: row.get(2)?,
        command: command
            .map(|c| serde_json::from_str(&c))
            .transpose()
            .map_err(StoreError::corrupt)?,
        token_env: row.get(5)?,
        created_at: parse_ts(&created)?,
    })
}

fn command_column(c: &Connector) -> Result<Option<String>, StoreError> {
    c.command
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(StoreError::corrupt)
}

fn not_found(id: Uuid) -> StoreError {
    StoreError::NotFound {
        entity: "connector",
        key: id.to_string(),
    }
}

fn conflict(name: &str, e: rusqlite::Error) -> StoreError {
    if is_unique_violation(&e) {
        StoreError::Conflict {
            entity: "connector",
            key: name.to_owned(),
        }
    } else {
        e.into()
    }
}

impl Store {
    pub async fn create_connector(&self, c: Connector) -> Result<Connector, StoreError> {
        let command = command_column(&c)?;
        self.with(move |conn| {
            conn.execute(
                "INSERT INTO connectors (id, name, url, created_at, command, token_env)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    c.id.to_string(),
                    c.name,
                    c.url,
                    fmt_ts(c.created_at),
                    command,
                    c.token_env
                ],
            )
            .map_err(|e| conflict(&c.name, e))?;
            Ok(c)
        })
        .await
    }

    /// Replaces `name`, `url`, `command` and `token_env`.
    pub async fn update_connector(&self, c: Connector) -> Result<(), StoreError> {
        let command = command_column(&c)?;
        self.with(move |conn| {
            let changed = conn
                .execute(
                    "UPDATE connectors SET name = ?2, url = ?3, command = ?4, token_env = ?5
                     WHERE id = ?1",
                    params![c.id.to_string(), c.name, c.url, command, c.token_env],
                )
                .map_err(|e| conflict(&c.name, e))?;
            if changed == 0 {
                return Err(not_found(c.id));
            }
            Ok(())
        })
        .await
    }

    pub async fn get_connector(&self, id: Uuid) -> Result<Connector, StoreError> {
        self.with(move |c| {
            c.query_row(
                &format!("SELECT {COLUMNS} FROM connectors WHERE id = ?1"),
                [id.to_string()],
                |r| Ok(from_row(r)),
            )
            .optional()?
            .ok_or_else(|| not_found(id))?
        })
        .await
    }

    /// All connectors, by name.
    pub async fn list_connectors(&self) -> Result<Vec<Connector>, StoreError> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!("SELECT {COLUMNS} FROM connectors ORDER BY name"))?;
            let rows = stmt.query_map([], |r| Ok(from_row(r)))?;
            rows.map(|r| r?).collect()
        })
        .await
    }

    /// Deletes a connector and its assignments.
    pub async fn delete_connector(&self, id: Uuid) -> Result<(), StoreError> {
        self.with(move |c| {
            match c.execute("DELETE FROM connectors WHERE id = ?1", [id.to_string()])? {
                0 => Err(not_found(id)),
                _ => Ok(()),
            }
        })
        .await
    }

    /// The connectors `agent` may use, by name.
    pub async fn agent_connectors(&self, agent: AgentId) -> Result<Vec<Connector>, StoreError> {
        self.with(move |c| {
            let mut stmt = c.prepare(
                "SELECT c.id, c.name, c.url, c.created_at, c.command, c.token_env FROM connectors c
                 JOIN agent_connectors a ON a.connector_id = c.id
                 WHERE a.agent_id = ?1 ORDER BY c.name",
            )?;
            let rows = stmt.query_map([agent.to_string()], |r| Ok(from_row(r)))?;
            rows.map(|r| r?).collect()
        })
        .await
    }

    /// Replaces the agent's connectors. Unknown ids are a [`StoreError::NotFound`].
    pub async fn set_agent_connectors(
        &self,
        agent: AgentId,
        connectors: Vec<Uuid>,
    ) -> Result<(), StoreError> {
        self.with(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "DELETE FROM agent_connectors WHERE agent_id = ?1",
                [agent.to_string()],
            )?;
            for id in connectors {
                let exists = tx
                    .query_row(
                        "SELECT 1 FROM connectors WHERE id = ?1",
                        [id.to_string()],
                        |_| Ok(()),
                    )
                    .optional()?;
                if exists.is_none() {
                    return Err(not_found(id));
                }
                tx.execute(
                    "INSERT OR IGNORE INTO agent_connectors (agent_id, connector_id) VALUES (?1, ?2)",
                    params![agent.to_string(), id.to_string()],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use ferrobot_core::AgentName;
    use time::OffsetDateTime;

    use super::*;
    use crate::NewAgent;

    fn connector(name: &str) -> Connector {
        Connector {
            id: Uuid::new_v4(),
            name: name.into(),
            url: "https://example.test/mcp".into(),
            command: None,
            token_env: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[tokio::test]
    async fn a_local_server_keeps_its_command() {
        let store = Store::open_in_memory().unwrap();
        let local = Connector {
            url: String::new(),
            command: Some(vec!["npx".into(), "-y".into(), "@scope/server".into()]),
            token_env: Some("API_TOKEN".into()),
            ..connector("files")
        };
        store.create_connector(local.clone()).await.unwrap();
        assert_eq!(store.get_connector(local.id).await.unwrap(), local);
    }

    #[tokio::test]
    async fn crud_and_assignment() {
        let store = Store::open_in_memory().unwrap();
        let gh = store.create_connector(connector("github")).await.unwrap();
        let db = store.create_connector(connector("db")).await.unwrap();
        assert!(matches!(
            store.create_connector(connector("github")).await,
            Err(StoreError::Conflict { .. })
        ));
        let mut moved = gh.clone();
        moved.url = "https://other.test/mcp".into();
        store.update_connector(moved.clone()).await.unwrap();
        assert_eq!(store.get_connector(gh.id).await.unwrap(), moved);
        assert_eq!(
            store.list_connectors().await.unwrap(),
            vec![db.clone(), moved.clone()]
        );

        let agent = store
            .create_agent(NewAgent {
                created_by: None,
                name: AgentName::parse("alice").unwrap(),
                description: String::new(),
                instructions: String::new(),
            })
            .await
            .unwrap()
            .id;
        store
            .set_agent_connectors(agent, vec![gh.id, db.id, gh.id])
            .await
            .unwrap();
        assert_eq!(
            store.agent_connectors(agent).await.unwrap(),
            vec![db.clone(), moved]
        );
        assert!(matches!(
            store
                .set_agent_connectors(agent, vec![Uuid::new_v4()])
                .await,
            Err(StoreError::NotFound { .. })
        ));
        assert_eq!(
            store.agent_connectors(agent).await.unwrap().len(),
            2,
            "a failed update changes nothing"
        );

        store.delete_connector(gh.id).await.unwrap();
        assert_eq!(store.agent_connectors(agent).await.unwrap(), vec![db]);
        assert!(matches!(
            store.delete_connector(gh.id).await,
            Err(StoreError::NotFound { .. })
        ));
    }
}
