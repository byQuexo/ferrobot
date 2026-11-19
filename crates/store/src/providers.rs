//! Model provider configuration. Credentials are not stored here.

use ferrobot_core::{Provider, ProviderKind};
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

use crate::util::{fmt_ts, is_unique_violation, parse_ts};
use crate::{Store, StoreError};

const COLUMNS: &str = "id, name, kind, base_url, region, project, default_model, created_at";

fn from_row(row: &Row<'_>) -> Result<Provider, StoreError> {
    let id: String = row.get(0)?;
    let kind: String = row.get(2)?;
    let created: String = row.get(7)?;
    Ok(Provider {
        id: Uuid::parse_str(&id).map_err(StoreError::corrupt)?,
        name: row.get(1)?,
        kind: kind.parse::<ProviderKind>()?,
        base_url: row.get(3)?,
        region: row.get(4)?,
        project: row.get(5)?,
        default_model: row.get(6)?,
        created_at: parse_ts(&created)?,
    })
}

fn not_found(id: Uuid) -> StoreError {
    StoreError::NotFound {
        entity: "provider",
        key: id.to_string(),
    }
}

fn conflict(name: &str, e: rusqlite::Error) -> StoreError {
    if is_unique_violation(&e) {
        StoreError::Conflict {
            entity: "provider",
            key: name.to_owned(),
        }
    } else {
        e.into()
    }
}

impl Store {
    pub async fn create_provider(&self, p: Provider) -> Result<Provider, StoreError> {
        self.with(move |c| {
            c.execute(
                "INSERT INTO providers (id, name, kind, base_url, region, project, default_model,
                                        created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    p.id.to_string(),
                    p.name,
                    p.kind.as_str(),
                    p.base_url,
                    p.region,
                    p.project,
                    p.default_model,
                    fmt_ts(p.created_at)
                ],
            )
            .map_err(|e| conflict(&p.name, e))?;
            Ok(p)
        })
        .await
    }

    /// Replaces every field except `id`, `kind` and `created_at`.
    pub async fn update_provider(&self, p: Provider) -> Result<(), StoreError> {
        self.with(move |c| {
            let changed = c
                .execute(
                    "UPDATE providers SET name = ?2, base_url = ?3, region = ?4, project = ?5,
                                          default_model = ?6
                     WHERE id = ?1",
                    params![
                        p.id.to_string(),
                        p.name,
                        p.base_url,
                        p.region,
                        p.project,
                        p.default_model
                    ],
                )
                .map_err(|e| conflict(&p.name, e))?;
            if changed == 0 {
                return Err(not_found(p.id));
            }
            Ok(())
        })
        .await
    }

    pub async fn get_provider(&self, id: Uuid) -> Result<Provider, StoreError> {
        self.with(move |c| {
            c.query_row(
                &format!("SELECT {COLUMNS} FROM providers WHERE id = ?1"),
                [id.to_string()],
                |r| Ok(from_row(r)),
            )
            .optional()?
            .ok_or_else(|| not_found(id))?
        })
        .await
    }

    /// All providers, by name.
    pub async fn list_providers(&self) -> Result<Vec<Provider>, StoreError> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!("SELECT {COLUMNS} FROM providers ORDER BY name"))?;
            let rows = stmt.query_map([], |r| Ok(from_row(r)))?;
            rows.map(|r| r?).collect()
        })
        .await
    }

    /// Deletes a provider; [`StoreError::Conflict`] while agents still use it.
    pub async fn delete_provider(&self, id: Uuid) -> Result<(), StoreError> {
        self.with(move |c| {
            let in_use: i64 = c.query_row(
                "SELECT COUNT(*) FROM agents WHERE provider_id = ?1",
                [id.to_string()],
                |r| r.get(0),
            )?;
            if in_use > 0 {
                return Err(StoreError::Conflict {
                    entity: "provider in use by agents",
                    key: id.to_string(),
                });
            }
            match c.execute("DELETE FROM providers WHERE id = ?1", [id.to_string()])? {
                0 => Err(not_found(id)),
                _ => Ok(()),
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use ferrobot_core::AgentName;
    use time::OffsetDateTime;

    use super::*;
    use crate::{AgentPatch, NewAgent};

    fn provider(name: &str) -> Provider {
        Provider {
            id: Uuid::new_v4(),
            name: name.into(),
            kind: ProviderKind::Openai,
            base_url: None,
            region: None,
            project: None,
            default_model: Some("gpt-test".into()),
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[tokio::test]
    async fn crud_and_in_use_guard() {
        let store = Store::open_in_memory().unwrap();
        let p = store.create_provider(provider("work")).await.unwrap();
        assert!(matches!(
            store.create_provider(provider("work")).await,
            Err(StoreError::Conflict { .. })
        ));
        let mut changed = p.clone();
        changed.default_model = None;
        changed.base_url = Some("https://example.test/v1".into());
        store.update_provider(changed.clone()).await.unwrap();
        assert_eq!(store.get_provider(p.id).await.unwrap(), changed);
        assert_eq!(store.list_providers().await.unwrap(), vec![changed]);

        let agent = store
            .create_agent(NewAgent {
                created_by: None,
                name: AgentName::parse("alice").unwrap(),
                description: String::new(),
                instructions: String::new(),
            })
            .await
            .unwrap();
        let patch = AgentPatch {
            provider_id: Some(Some(p.id)),
            model: Some(Some("gpt-x".into())),
            ..AgentPatch::default()
        };
        store.update_agent_profile(agent.id, patch).await.unwrap();
        let agent = store.get_agent(agent.id).await.unwrap();
        assert_eq!(
            (agent.provider_id, agent.model.as_deref()),
            (Some(p.id), Some("gpt-x"))
        );
        assert!(matches!(
            store.delete_provider(p.id).await,
            Err(StoreError::Conflict { .. })
        ));

        let clear = AgentPatch {
            provider_id: Some(None),
            ..AgentPatch::default()
        };
        store.update_agent_profile(agent.id, clear).await.unwrap();
        let agent = store.get_agent(agent.id).await.unwrap();
        assert_eq!(
            (agent.provider_id, agent.model.as_deref()),
            (None, Some("gpt-x"))
        );
        store.delete_provider(p.id).await.unwrap();
        assert!(matches!(
            store.get_provider(p.id).await,
            Err(StoreError::NotFound { .. })
        ));
    }
}
