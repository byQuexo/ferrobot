use std::collections::HashMap;

use ferrobot_core::{Agent, AgentId, AgentName, AgentStatus, AvatarCharacter};
use rusqlite::{OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

use crate::util::{fmt_ts, is_unique_violation, now, parse_ts};
use crate::{Store, StoreError};

/// Input for registering an agent.
#[derive(Debug, Clone)]
pub struct NewAgent {
    pub name: AgentName,
    pub description: String,
    pub instructions: String,
    /// Creating agent; `None`: the operator.
    pub created_by: Option<AgentId>,
}

/// Partial profile update; `None` leaves a field unchanged.
#[derive(Debug, Clone, Default)]
pub struct AgentPatch {
    /// Rename. Must stay unique; the sandbox keeps its original name.
    pub name: Option<AgentName>,
    pub description: Option<String>,
    pub instructions: Option<String>,
    /// `Some(None)` clears the chosen character look.
    pub character: Option<Option<AvatarCharacter>>,
    /// `Some(None)` goes back to the daemon's default provider.
    pub provider_id: Option<Option<uuid::Uuid>>,
    /// `Some(None)` goes back to the provider's default model.
    pub model: Option<Option<String>>,
    pub pinned: Option<bool>,
}

/// An avatar image as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Avatar {
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// SHA-256 of a runner token. Only hashes are stored.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

const COLUMNS: &str = "id, name, instructions, status, backend, sandbox_id, session_id, created_at, \
                       description, avatar_rev, COALESCE(sandbox_name, name), character, created_by, \
                       provider_id, model, pinned";

fn from_row(row: &Row<'_>) -> Result<Agent, StoreError> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let status: String = row.get(3)?;
    let created: String = row.get(7)?;
    let character: Option<String> = row.get(11)?;
    let created_by: String = row.get(12)?;
    let provider_id: Option<String> = row.get(13)?;
    Ok(Agent {
        provider_id: provider_id
            .map(|id| uuid::Uuid::parse_str(&id))
            .transpose()
            .map_err(StoreError::corrupt)?,
        model: row.get(14)?,
        pinned: row.get(15)?,
        created_by: match created_by.as_str() {
            OPERATOR => None,
            id => Some(id.parse()?),
        },
        character: character
            .map(|c| serde_json::from_str(&c))
            .transpose()
            .map_err(StoreError::corrupt)?,
        id: id.parse()?,
        name: AgentName::parse(name)?,
        sandbox_name: row.get(10)?,
        description: row.get(8)?,
        instructions: row.get(2)?,
        avatar_rev: row.get(9)?,
        status: status.parse()?,
        backend: row.get(4)?,
        sandbox_id: row.get(5)?,
        session_id: row.get(6)?,
        created_at: parse_ts(&created)?,
    })
}

/// `created_by` value of agents the operator created.
const OPERATOR: &str = "operator";

fn not_found(key: &impl std::fmt::Display) -> StoreError {
    StoreError::NotFound {
        entity: "agent",
        key: key.to_string(),
    }
}

impl Store {
    pub async fn create_agent(&self, new: NewAgent) -> Result<Agent, StoreError> {
        let id = AgentId::new();
        let agent = Agent {
            id,
            sandbox_name: new.name.to_string(),
            name: new.name,
            description: new.description,
            instructions: new.instructions,
            avatar_rev: 0,
            character: None,
            created_by: new.created_by,
            provider_id: None,
            model: None,
            status: AgentStatus::Created,
            backend: None,
            sandbox_id: None,
            session_id: None,
            pinned: false,
            created_at: now(),
        };
        let mut row = agent;
        self.with(move |c| {
            // A renamed agent may still own sandbox resources under this name.
            let taken: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM agents WHERE sandbox_name = ?1)",
                [&row.sandbox_name],
                |r| r.get(0),
            )?;
            if taken {
                let short = id.to_string();
                row.sandbox_name = format!("{}-{}", row.name, &short[..8]);
            }
            c.execute(
                "INSERT INTO agents (id, name, description, instructions, status, created_at,
                                     sandbox_name, created_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    row.id.to_string(),
                    row.name.as_str(),
                    row.description,
                    row.instructions,
                    row.status.as_str(),
                    fmt_ts(row.created_at),
                    row.sandbox_name,
                    row.created_by
                        .map_or_else(|| OPERATOR.to_owned(), |id| id.to_string()),
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
            Ok(row)
        })
        .await
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
            .ok_or_else(|| not_found(&id))?
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

    pub async fn set_agent_status(
        &self,
        id: AgentId,
        status: AgentStatus,
    ) -> Result<(), StoreError> {
        self.update_agent(id, move |c, id| {
            c.execute(
                "UPDATE agents SET status = ?2 WHERE id = ?1",
                params![id, status.as_str()],
            )
        })
        .await
    }

    pub async fn set_agent_session(
        &self,
        id: AgentId,
        session_id: String,
    ) -> Result<(), StoreError> {
        self.update_agent(id, move |c, id| {
            c.execute(
                "UPDATE agents SET session_id = ?2 WHERE id = ?1",
                params![id, session_id],
            )
        })
        .await
    }

    pub async fn delete_agent(&self, id: AgentId) -> Result<(), StoreError> {
        self.update_agent(id, |c, id| {
            c.execute("DELETE FROM agents WHERE id = ?1", [id])
        })
        .await
    }

    /// Applies `patch`; a rename onto a taken name is a [`StoreError::Conflict`].
    pub async fn update_agent_profile(
        &self,
        id: AgentId,
        patch: AgentPatch,
    ) -> Result<(), StoreError> {
        let name = patch.name.map(String::from);
        let key = name.clone().unwrap_or_default();
        let set_character = patch.character.is_some();
        let set_provider = patch.provider_id.is_some();
        let provider_id = patch.provider_id.flatten().map(|id| id.to_string());
        let set_model = patch.model.is_some();
        let model = patch.model.flatten();
        let character = patch
            .character
            .flatten()
            .map(|c| serde_json::to_string(&c))
            .transpose()
            .map_err(StoreError::corrupt)?;
        self.update_agent(id, move |c, id| {
            c.execute(
                "UPDATE agents SET name = COALESCE(?2, name),
                                   description = COALESCE(?3, description),
                                   instructions = COALESCE(?4, instructions),
                                   character = CASE WHEN ?5 THEN ?6 ELSE character END,
                                   provider_id = CASE WHEN ?7 THEN ?8 ELSE provider_id END,
                                   model = CASE WHEN ?9 THEN ?10 ELSE model END,
                                   pinned = COALESCE(?11, pinned)
                 WHERE id = ?1",
                params![
                    id,
                    name,
                    patch.description,
                    patch.instructions,
                    set_character,
                    character,
                    set_provider,
                    provider_id,
                    set_model,
                    model,
                    patch.pinned
                ],
            )
        })
        .await
        .map_err(|e| match e {
            StoreError::Sqlite(ref inner) if is_unique_violation(inner) => StoreError::Conflict {
                entity: "agent",
                key,
            },
            other => other,
        })
    }

    /// Sets or (with `None`) removes the avatar. `avatar_rev` changes on every
    /// call (so URLs can cache-bust) and is positive only while an avatar exists.
    pub async fn set_avatar(&self, id: AgentId, avatar: Option<Avatar>) -> Result<(), StoreError> {
        self.update_agent(id, move |c, id| {
            let (mime, bytes) = avatar.map(|a| (a.mime, a.bytes)).unzip();
            c.execute(
                "UPDATE agents SET avatar = ?2, avatar_mime = ?3,
                    avatar_rev = (abs(avatar_rev) + 1) * CASE WHEN ?2 IS NULL THEN -1 ELSE 1 END
                 WHERE id = ?1",
                params![id, bytes, mime],
            )
        })
        .await
    }

    pub async fn get_avatar(&self, id: AgentId) -> Result<Option<Avatar>, StoreError> {
        self.with(move |c| {
            let row: Option<(Option<String>, Option<Vec<u8>>)> = c
                .query_row(
                    "SELECT avatar_mime, avatar FROM agents WHERE id = ?1",
                    [id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (mime, bytes) = row.ok_or_else(|| not_found(&id))?;
            Ok(mime.zip(bytes).map(|(mime, bytes)| Avatar { mime, bytes }))
        })
        .await
    }

    /// Marks everything the agent produced so far as read.
    pub async fn mark_read(&self, id: AgentId) -> Result<(), StoreError> {
        self.update_agent(id, |c, id| {
            c.execute(
                "UPDATE agents SET last_read_event_id =
                    (SELECT COALESCE(MAX(id), 0) FROM events WHERE agent_id = ?1)
                 WHERE id = ?1",
                [id],
            )
        })
        .await
    }

    /// Unread finished turns per agent (only agents with unread > 0).
    pub async fn unread_counts(&self) -> Result<HashMap<AgentId, u32>, StoreError> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT a.id, COUNT(e.id) FROM agents a
                 JOIN events e ON e.agent_id = a.id
                 WHERE e.id > a.last_read_event_id AND e.kind = 'turn_done'
                 GROUP BY a.id",
            )?;
            let mut rows = stmt.query([])?;
            let mut out = HashMap::new();
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let n: u32 = row.get(1)?;
                out.insert(id.parse()?, n);
            }
            Ok(out)
        })
        .await
    }

    async fn update_agent<F>(&self, id: AgentId, f: F) -> Result<(), StoreError>
    where
        F: FnOnce(&rusqlite::Connection, String) -> rusqlite::Result<usize> + Send + 'static,
    {
        self.with(move |c| match f(c, id.to_string())? {
            0 => Err(not_found(&id)),
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
            created_by: None,
            name: AgentName::parse(name).unwrap(),
            description: String::new(),
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
    async fn rename_keeps_sandbox_name() {
        let store = Store::open_in_memory().unwrap();
        let a = store.create_agent(new("alice")).await.unwrap();
        store.create_agent(new("bob")).await.unwrap();
        let rename = |n: &str| AgentPatch {
            name: Some(AgentName::parse(n).unwrap()),
            ..AgentPatch::default()
        };
        assert!(matches!(
            store.update_agent_profile(a.id, rename("bob")).await,
            Err(StoreError::Conflict { .. })
        ));
        store
            .update_agent_profile(a.id, rename("ada"))
            .await
            .unwrap();
        let got = store.get_agent(a.id).await.unwrap();
        assert_eq!(
            (got.name.as_str(), got.sandbox_name.as_str()),
            ("ada", "alice")
        );
        // The old name is free again, but its sandbox name is still taken.
        let c = store.create_agent(new("alice")).await.unwrap();
        assert_eq!(c.name.as_str(), "alice");
        assert!(c.sandbox_name.starts_with("alice-"), "{}", c.sandbox_name);
        assert_eq!(store.get_agent(c.id).await.unwrap(), c);
    }

    #[tokio::test]
    async fn token_lookup_uses_hash_and_rotates() {
        let store = Store::open_in_memory().unwrap();
        let a = store.create_agent(new("alice")).await.unwrap();
        store.set_agent_token(a.id, "secret-1").await.unwrap();
        assert_eq!(
            store.agent_by_token("secret-1").await.unwrap().unwrap().id,
            a.id
        );
        store.set_agent_token(a.id, "secret-2").await.unwrap();
        assert!(store.agent_by_token("secret-1").await.unwrap().is_none());
        assert!(store.agent_by_token("secret-2").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn status_and_session_updates() {
        let store = Store::open_in_memory().unwrap();
        let a = store.create_agent(new("alice")).await.unwrap();
        store
            .set_agent_status(a.id, AgentStatus::Running)
            .await
            .unwrap();
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

    #[tokio::test]
    async fn profile_avatar_and_unread() {
        let store = Store::open_in_memory().unwrap();
        let a = store.create_agent(new("alice")).await.unwrap();
        store
            .update_agent_profile(
                a.id,
                AgentPatch {
                    description: Some("Reviews code".into()),
                    ..AgentPatch::default()
                },
            )
            .await
            .unwrap();
        let got = store.get_agent(a.id).await.unwrap();
        assert_eq!(got.description, "Reviews code");
        assert_eq!(got.instructions, "test");

        assert_eq!(store.get_avatar(a.id).await.unwrap(), None);
        let png = Avatar {
            mime: "image/png".into(),
            bytes: vec![1, 2, 3],
        };
        store.set_avatar(a.id, Some(png.clone())).await.unwrap();
        store.set_avatar(a.id, Some(png.clone())).await.unwrap();
        assert_eq!(store.get_avatar(a.id).await.unwrap(), Some(png));
        assert_eq!(store.get_agent(a.id).await.unwrap().avatar_rev, 2);
        store.set_avatar(a.id, None).await.unwrap();
        assert_eq!(store.get_agent(a.id).await.unwrap().avatar_rev, -3);

        let none: HashMap<AgentId, u32> = HashMap::new();
        assert_eq!(store.unread_counts().await.unwrap(), none);
        for kind in ["assistant_text", "turn_done", "turn_done"] {
            store
                .append_event(a.id, None, kind.into(), serde_json::json!({}))
                .await
                .unwrap();
        }
        assert_eq!(store.unread_counts().await.unwrap()[&a.id], 2);
        store.mark_read(a.id).await.unwrap();
        assert_eq!(store.unread_counts().await.unwrap(), none);
    }
}
