use std::collections::HashMap;

use ferrobot_core::{Agent, AgentId, AgentName, AgentStatus, AvatarCharacter, ToolGroup};
use rusqlite::{Connection, OptionalExtension, Params, Row, params};
use sha2::{Digest, Sha256};

use crate::events::insert_event;
use crate::util::{conflict_or, fmt_ts, is_unique_violation, now, parse_ts};
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
    pub tools: Option<Vec<ToolGroup>>,
    /// Validated by the caller; empty removes every tag.
    pub tags: Option<Vec<String>>,
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
                       provider_id, model, pinned, tools, tags";

fn from_row(row: &Row<'_>) -> Result<Agent, StoreError> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let status: String = row.get(3)?;
    let created: String = row.get(7)?;
    let character: Option<String> = row.get(11)?;
    let created_by: String = row.get(12)?;
    let provider_id: Option<String> = row.get(13)?;
    let tools: Option<String> = row.get(16)?;
    let tags: Option<String> = row.get(17)?;
    Ok(Agent {
        tags: tags
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default(),
        tools: tools.map_or_else(ToolGroup::all, |t| parse_tools(&t)),
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

/// Groups this build does not know (written by a newer daemon) are dropped: narrower, not wider.
fn parse_tools(json: &str) -> Vec<ToolGroup> {
    let names: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    let groups = names
        .into_iter()
        .filter_map(|n| serde_json::from_value(n).ok())
        .collect();
    ToolGroup::normalize(groups)
}

/// `tools` as stored: NULL for every group, so groups added later reach those agents too.
fn tools_column(tools: Vec<ToolGroup>) -> Result<Option<String>, StoreError> {
    let tools = ToolGroup::normalize(tools);
    if tools == ToolGroup::all() {
        return Ok(None);
    }
    serde_json::to_string(&tools)
        .map(Some)
        .map_err(StoreError::corrupt)
}

/// `created_by` value of agents the operator created.
const OPERATOR: &str = "operator";

fn not_found(key: &impl std::fmt::Display) -> StoreError {
    StoreError::not_found("agent", key)
}

fn find(c: &Connection, filter: &str, params: impl Params) -> Result<Option<Agent>, StoreError> {
    c.query_row(
        &format!("SELECT {COLUMNS} FROM agents WHERE {filter}"),
        params,
        |row| Ok(from_row(row)),
    )
    .optional()?
    .transpose()
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
            tools: ToolGroup::all(),
            tags: Vec::new(),
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
            .map_err(|e| conflict_or(e, "agent", row.name.as_str()))?;
            // A new agent takes over a name another agent had before a rename.
            c.execute(
                "DELETE FROM agent_aliases WHERE name = ?1",
                [row.name.as_str()],
            )?;
            Ok(row)
        })
        .await
    }

    pub async fn list_agents(&self) -> Result<Vec<Agent>, StoreError> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!("SELECT {COLUMNS} FROM agents ORDER BY name"))?;
            let rows = stmt.query_map([], |r| Ok(from_row(r)))?;
            rows.map(|r| r?).collect()
        })
        .await
    }

    pub async fn get_agent(&self, id: AgentId) -> Result<Agent, StoreError> {
        self.with(move |c| find(c, "id = ?1", [id.to_string()])?.ok_or_else(|| not_found(&id)))
            .await
    }

    /// The agent with this name, or that had it before a rename (a current name wins).
    pub async fn get_agent_by_name(&self, name: AgentName) -> Result<Agent, StoreError> {
        self.with(move |c| {
            find(
                c,
                "name = ?1 OR id = (SELECT agent_id FROM agent_aliases WHERE name = ?1)
                 ORDER BY name = ?1 DESC LIMIT 1",
                [name.as_str()],
            )?
            .ok_or_else(|| not_found(&name))
        })
        .await
    }

    /// Resolves a runner token to its agent. Returns `None` for unknown tokens.
    pub async fn agent_by_token(&self, token: &str) -> Result<Option<Agent>, StoreError> {
        let hash = hash_token(token);
        self.with(move |c| find(c, "token_hash = ?1", [hash])).await
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
        session_id: Option<String>,
    ) -> Result<(), StoreError> {
        self.update_agent(id, move |c, id| {
            c.execute(
                "UPDATE agents SET session_id = ?2 WHERE id = ?1",
                params![id, session_id],
            )
        })
        .await
    }

    /// Deletes the agent; its name is kept (`former_agent_names`) so mentions of it still read.
    pub async fn delete_agent(&self, id: AgentId) -> Result<(), StoreError> {
        let at = fmt_ts(now());
        self.update_agent(id, move |c, id| {
            c.execute(
                "INSERT INTO settings (key, value, updated_at)
                 SELECT 'former_agents', json_object(id, name), ?2 FROM agents WHERE id = ?1
                 ON CONFLICT (key) DO UPDATE SET
                     value = json_set(settings.value, '$.\"' || ?1 || '\"',
                                      (SELECT name FROM agents WHERE id = ?1)),
                     updated_at = excluded.updated_at",
                params![id, at],
            )?;
            c.execute("DELETE FROM agents WHERE id = ?1", [id])
        })
        .await
    }

    /// The last names of deleted agents: recorded when they were deleted, else the names
    /// history knew them by (messages they sent, `create_agent` results).
    pub async fn former_agent_names(&self) -> Result<Vec<(AgentId, String)>, StoreError> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "WITH seen(agent_id, name, at) AS (
                     SELECT json_extract(payload, '$.sender.id'), json_extract(payload, '$.from'), id
                     FROM events WHERE kind = 'message_in'
                       AND json_extract(payload, '$.sender.kind') = 'agent'
                     UNION ALL
                     SELECT json_extract(json_extract(payload, '$.output'), '$.agent_id'),
                            json_extract(json_extract(payload, '$.output'), '$.name'), id
                     FROM events WHERE kind = 'tool_result'
                       AND json_valid(json_extract(payload, '$.output'))
                     UNION ALL
                     SELECT j.key, j.value, 9223372036854775807
                     FROM settings s, json_each(s.value) j WHERE s.key = 'former_agents'
                 )
                 SELECT agent_id, name, MAX(at) FROM seen
                 WHERE agent_id IS NOT NULL AND name IS NOT NULL
                   AND agent_id NOT IN (SELECT id FROM agents)
                 GROUP BY agent_id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                let (id, name) = row?;
                if let Ok(id) = id.parse() {
                    out.push((id, name));
                }
            }
            Ok(out)
        })
        .await
    }

    /// Records the old names history shows for agents renamed before renames were recorded, as
    /// aliases plus `renamed` events (names held now are skipped); returns how many.
    pub async fn backfill_aliases(&self) -> Result<usize, StoreError> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "WITH seen(name, agent_id) AS (
                     SELECT json_extract(payload, '$.from'), json_extract(payload, '$.sender.id')
                     FROM events WHERE kind = 'message_in'
                       AND json_extract(payload, '$.sender.kind') = 'agent'
                     UNION
                     SELECT json_extract(json_extract(payload, '$.output'), '$.name'),
                            json_extract(json_extract(payload, '$.output'), '$.agent_id')
                     FROM events WHERE kind = 'tool_result'
                       AND json_valid(json_extract(payload, '$.output'))
                 )
                 SELECT DISTINCT s.name, a.id, a.name FROM seen s JOIN agents a ON a.id = s.agent_id
                 WHERE s.name IS NOT NULL AND s.name <> a.name
                   AND s.name NOT IN (SELECT name FROM agents)
                   AND s.name NOT IN (SELECT name FROM agent_aliases)",
            )?;
            let found: Vec<(String, String, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?;
            let tx = c.unchecked_transaction()?;
            let mut added = 0;
            for (old, id, now_name) in found {
                if AgentName::parse(old.clone()).is_err() {
                    continue;
                }
                let inserted = tx.execute(
                    "INSERT OR IGNORE INTO agent_aliases (name, agent_id) VALUES (?1, ?2)",
                    params![old, id],
                )?;
                if inserted == 0 {
                    continue;
                }
                let payload = serde_json::json!({ "from": old, "to": now_name }).to_string();
                insert_event(&tx, &id, None, "renamed", &payload)?;
                added += 1;
            }
            tx.commit()?;
            Ok(added)
        })
        .await
    }

    /// Old names of renamed agents: (name, agent).
    pub async fn agent_aliases(&self) -> Result<Vec<(String, AgentId)>, StoreError> {
        self.with(|c| {
            let mut stmt = c.prepare("SELECT name, agent_id FROM agent_aliases")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                let (name, id) = row?;
                out.push((name, id.parse().map_err(StoreError::corrupt)?));
            }
            Ok(out)
        })
        .await
    }

    /// Applies `patch`; a rename onto a taken name is a [`StoreError::Conflict`]. The old name
    /// stays an alias, so `@old-name` still finds the agent.
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
        let set_tools = patch.tools.is_some();
        let tools = patch.tools.map(tools_column).transpose()?.flatten();
        let set_tags = patch.tags.is_some();
        let tags = patch
            .tags
            .filter(|t| !t.is_empty())
            .map(|t| serde_json::to_string(&t))
            .transpose()
            .map_err(StoreError::corrupt)?;
        self.update_agent(id, move |c, id| {
            let old: Option<String> = c
                .query_row("SELECT name FROM agents WHERE id = ?1", [&id], |r| r.get(0))
                .optional()?;
            let renamed = name
                .clone()
                .filter(|n| old.as_ref().is_some_and(|o| o != n));
            let changed = c.execute(
                "UPDATE agents SET name = COALESCE(?2, name),
                                   description = COALESCE(?3, description),
                                   instructions = COALESCE(?4, instructions),
                                   character = CASE WHEN ?5 THEN ?6 ELSE character END,
                                   provider_id = CASE WHEN ?7 THEN ?8 ELSE provider_id END,
                                   model = CASE WHEN ?9 THEN ?10 ELSE model END,
                                   pinned = COALESCE(?11, pinned),
                                   tools = CASE WHEN ?12 THEN ?13 ELSE tools END,
                                   tags = CASE WHEN ?14 THEN ?15 ELSE tags END
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
                    patch.pinned,
                    set_tools,
                    tools,
                    set_tags,
                    tags
                ],
            )?;
            if let (Some(new), Some(old)) = (renamed, old) {
                c.execute("DELETE FROM agent_aliases WHERE name = ?1", [&new])?;
                c.execute(
                    "INSERT OR REPLACE INTO agent_aliases (name, agent_id) VALUES (?1, ?2)",
                    [&old, &id],
                )?;
                // Best effort: a failed move leaves the notes under the old name, not lost.
                let _ = crate::vault::rename_folder(c, &old, &new);
            }
            Ok(changed)
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

    /// Unread finished turns per agent (only agents with unread > 0); call room turns are not in
    /// the agent's chat, so they do not count.
    pub async fn unread_counts(&self) -> Result<HashMap<AgentId, u32>, StoreError> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT a.id, COUNT(e.id) FROM agents a
                 JOIN events e ON e.agent_id = a.id
                 WHERE e.id > a.last_read_event_id AND e.kind = 'turn_done'
                   AND NOT EXISTS (SELECT 1 FROM events m WHERE m.turn_id = e.turn_id
                       AND m.kind = 'message_in' AND json_extract(m.payload, '$.room_id') IS NOT NULL)
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

    /// Question cards per agent that are still waiting for the operator's answer.
    pub async fn waiting_counts(&self) -> Result<HashMap<AgentId, u32>, StoreError> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT q.agent_id, COUNT(*) FROM events q WHERE q.kind = 'question'
                   AND NOT EXISTS (SELECT 1 FROM events a WHERE a.agent_id = q.agent_id
                       AND a.kind = 'question_answered'
                       AND json_extract(a.payload, '$.question') = q.id)
                 GROUP BY q.agent_id",
            )?;
            let mut rows = stmt.query([])?;
            let mut out = HashMap::new();
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                out.insert(id.parse()?, row.get(1)?);
            }
            Ok(out)
        })
        .await
    }

    /// Each agent's newest reply text and when it came; a call's lines are not replies.
    pub async fn last_replies(&self) -> Result<HashMap<AgentId, (String, String)>, StoreError> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT e.agent_id, json_extract(e.payload, '$.text'), e.created_at FROM events e
                 WHERE e.id IN (SELECT MAX(x.id) FROM events x WHERE x.kind = 'assistant_text'
                     AND NOT EXISTS (SELECT 1 FROM events m WHERE m.turn_id = x.turn_id
                         AND m.kind = 'message_in' AND json_extract(m.payload, '$.room_id') IS NOT NULL)
                     GROUP BY x.agent_id)",
            )?;
            let mut rows = stmt.query([])?;
            let mut out = HashMap::new();
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let text: Option<String> = row.get(1)?;
                out.insert(id.parse()?, (text.unwrap_or_default(), row.get(2)?));
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
        store
            .set_agent_session(a.id, Some("s-1".into()))
            .await
            .unwrap();
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

    #[tokio::test]
    async fn open_questions_and_the_newest_reply() {
        let store = Store::open_in_memory().unwrap();
        let a = store.create_agent(new("alice")).await.unwrap();
        let ev = |kind: &str, payload: serde_json::Value| {
            store.append_event(a.id, None, kind.into(), payload)
        };
        ev("assistant_text", serde_json::json!({"text": "first"}))
            .await
            .unwrap();
        let q1 = ev("question", serde_json::json!({})).await.unwrap();
        ev("question", serde_json::json!({})).await.unwrap();
        ev("assistant_text", serde_json::json!({"text": "second"}))
            .await
            .unwrap();
        // A line in a call is not a reply.
        let call = Some("call-turn".to_owned());
        let room = serde_json::json!({"room_id": 1});
        store
            .append_event(a.id, call.clone(), "message_in".into(), room)
            .await
            .unwrap();
        let pass = serde_json::json!({"text": "(pass)"});
        store
            .append_event(a.id, call, "assistant_text".into(), pass)
            .await
            .unwrap();
        assert_eq!(store.waiting_counts().await.unwrap()[&a.id], 2);
        ev("question_answered", serde_json::json!({"question": q1.id}))
            .await
            .unwrap();
        assert_eq!(store.waiting_counts().await.unwrap()[&a.id], 1);
        assert_eq!(store.last_replies().await.unwrap()[&a.id].0, "second");
    }

    #[tokio::test]
    async fn tools_roundtrip_and_every_group_stays_null() {
        let store = Store::open_in_memory().unwrap();
        let a = store.create_agent(new("alice")).await.unwrap();
        assert_eq!(a.tools, ToolGroup::all());
        let set = |tools: Vec<ToolGroup>| AgentPatch {
            tools: Some(tools),
            ..AgentPatch::default()
        };
        let narrow = vec![ToolGroup::Shell, ToolGroup::Team, ToolGroup::Team];
        store.update_agent_profile(a.id, set(narrow)).await.unwrap();
        let tools = store.get_agent(a.id).await.unwrap().tools;
        assert_eq!(tools, [ToolGroup::Team, ToolGroup::Shell]);
        store
            .update_agent_profile(a.id, set(ToolGroup::all()))
            .await
            .unwrap();
        let stored: Option<String> = store
            .with(move |c| Ok(c.query_row("SELECT tools FROM agents", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(stored, None);
        assert_eq!(parse_tools(r#"["team","later_group"]"#), [ToolGroup::Team]);
    }

    #[tokio::test]
    async fn tags_roundtrip_and_none_stays_null() {
        let store = Store::open_in_memory().unwrap();
        let a = store.create_agent(new("alice")).await.unwrap();
        let set = |tags: &[&str]| AgentPatch {
            tags: Some(tags.iter().map(|t| (*t).to_owned()).collect()),
            ..AgentPatch::default()
        };
        store
            .update_agent_profile(a.id, set(&["ops", "Nightly"]))
            .await
            .unwrap();
        assert_eq!(
            store.get_agent(a.id).await.unwrap().tags,
            ["ops", "Nightly"]
        );
        store.update_agent_profile(a.id, set(&[])).await.unwrap();
        let stored: Option<String> = store
            .with(move |c| Ok(c.query_row("SELECT tags FROM agents", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(stored, None);
        assert_eq!(
            store.get_agent(a.id).await.unwrap().tags,
            Vec::<String>::new()
        );
    }

    #[tokio::test]
    async fn an_old_name_finds_the_renamed_agent_until_someone_takes_it() {
        let store = Store::open_in_memory().unwrap();
        let ann = store.create_agent(new("ann")).await.unwrap();
        let name = |n: &str| AgentName::parse(n).unwrap();
        let rename = |to: &str| AgentPatch {
            name: Some(name(to)),
            ..AgentPatch::default()
        };
        store
            .update_agent_profile(ann.id, rename("anna"))
            .await
            .unwrap();
        assert_eq!(
            store.get_agent_by_name(name("ann")).await.unwrap().id,
            ann.id
        );
        assert_eq!(
            store.agent_aliases().await.unwrap(),
            [("ann".to_owned(), ann.id)]
        );

        let other = store.create_agent(new("ann")).await.unwrap();
        assert_eq!(
            store.get_agent_by_name(name("ann")).await.unwrap().id,
            other.id
        );
        assert_eq!(
            store.agent_aliases().await.unwrap(),
            [],
            "a current name wins"
        );

        store
            .update_agent_profile(ann.id, rename("annie"))
            .await
            .unwrap();
        store.delete_agent(ann.id).await.unwrap();
        assert_eq!(
            store.agent_aliases().await.unwrap(),
            [],
            "gone with the agent"
        );
    }

    #[tokio::test]
    async fn old_names_from_before_aliases_are_found_in_history() {
        let store = Store::open_in_memory().unwrap();
        let new = |name: &str| NewAgent {
            created_by: None,
            name: AgentName::parse(name).unwrap(),
            description: String::new(),
            instructions: String::new(),
        };
        let mailer = store.create_agent(new("mailer-mails")).await.unwrap().id;
        let claude = store.create_agent(new("claude")).await.unwrap().id;
        let sent = |from: &str, id: AgentId| serde_json::json!({ "sender": { "kind": "agent", "id": id }, "from": from });
        for (kind, payload) in [
            ("message_in", sent("courier", mailer)),
            ("message_in", sent("mailer-mails", mailer)),
            ("message_in", sent("claude", mailer)),
            (
                "tool_result",
                serde_json::json!({
                    "output": format!(r#"{{"agent_id":"{mailer}","name":"courier"}}"#)
                }),
            ),
        ] {
            store
                .append_event(claude, None, kind.into(), payload)
                .await
                .unwrap();
        }
        assert_eq!(store.backfill_aliases().await.unwrap(), 1);
        assert_eq!(
            store.agent_aliases().await.unwrap(),
            [("courier".to_owned(), mailer)],
            "a name another agent holds now is not an alias"
        );
        let found = store
            .get_agent_by_name(AgentName::parse("courier").unwrap())
            .await
            .unwrap();
        assert_eq!(found.id, mailer);
        assert_eq!(
            store.renames_since_last_turn(claude).await.unwrap(),
            [(mailer, "courier".to_owned())],
            "agents that knew it hear of it"
        );
        assert_eq!(store.backfill_aliases().await.unwrap(), 0, "only once");
    }

    #[tokio::test]
    async fn deleted_agents_keep_their_names_for_mentions() {
        let store = Store::open_in_memory().unwrap();
        let new = |name: &str| NewAgent {
            created_by: None,
            name: AgentName::parse(name).unwrap(),
            description: String::new(),
            instructions: String::new(),
        };
        let ada = store.create_agent(new("ada")).await.unwrap().id;
        let bob = store.create_agent(new("bob")).await.unwrap().id;
        let cy = store.create_agent(new("cy")).await.unwrap().id;
        let gone_long_ago = AgentId::new();
        let sent = serde_json::json!({ "sender": { "kind": "agent", "id": gone_long_ago }, "from": "calc-a" });
        store
            .append_event(cy, None, "message_in".into(), sent)
            .await
            .unwrap();
        store.delete_agent(ada).await.unwrap();
        store.delete_agent(bob).await.unwrap();
        let mut names = store.former_agent_names().await.unwrap();
        names.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(
            names,
            [
                (ada, "ada".to_owned()),
                (bob, "bob".to_owned()),
                (gone_long_ago, "calc-a".to_owned()),
            ]
        );
    }
}
