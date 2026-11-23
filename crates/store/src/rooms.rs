//! Rooms: a voice call with two or more agents, or a named group chat; who is in it and what
//! was said.

use ferrobot_core::{AgentId, Sender};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use time::OffsetDateTime;

use crate::util::{fmt_ts, now, parse_ts};
use crate::{Store, StoreError};

/// A voice call with two or more agents, kept as history once the call ended; or, with a name,
/// a group chat, kept until deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct Room {
    pub id: i64,
    /// The group's name; `null`: a call room.
    pub name: Option<String>,
    /// Every agent that took part (a group: its members), in the order they joined.
    pub members: Vec<AgentId>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
    /// When the call ended; `null` while it runs, and always for a group.
    #[serde(with = "time::serde::rfc3339::option")]
    #[cfg_attr(feature = "schema", schema(value_type = Option<String>, format = DateTime))]
    pub ended_at: Option<OffsetDateTime>,
}

/// One thing said in a room, by the operator (`user`) or an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct RoomLine {
    pub id: i64,
    pub room_id: i64,
    pub speaker: Sender,
    /// The words; mentions are `<@agent-id>` tokens. With `invited`, the topic that agent got.
    pub body: String,
    /// The agent this line brought into the call; `null` for words said.
    #[cfg_attr(feature = "schema", schema(value_type = Option<String>, format = Uuid))]
    pub invited: Option<AgentId>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

fn room(c: &Connection, id: i64) -> Result<Room, StoreError> {
    let row = c
        .query_row(
            "SELECT created_at, ended_at, name FROM rooms WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((created, ended, name)) = row else {
        return Err(StoreError::NotFound {
            entity: "room",
            key: id.to_string(),
        });
    };
    let mut stmt = c.prepare(
        "SELECT agent_id FROM room_members WHERE room_id = ?1 ORDER BY joined_at, rowid",
    )?;
    let mut rows = stmt.query([id])?;
    let mut members = Vec::new();
    while let Some(row) = rows.next()? {
        members.push(row.get::<_, String>(0)?.parse()?);
    }
    Ok(Room {
        id,
        name,
        members,
        created_at: parse_ts(&created)?,
        ended_at: ended.as_deref().map(parse_ts).transpose()?,
    })
}

fn line(row: &Row<'_>) -> Result<RoomLine, StoreError> {
    Ok(RoomLine {
        id: row.get(0)?,
        room_id: row.get(1)?,
        speaker: row.get::<_, String>(2)?.parse()?,
        body: row.get(3)?,
        created_at: parse_ts(&row.get::<_, String>(4)?)?,
        invited: row
            .get::<_, Option<String>>(5)?
            .map(|id| id.parse())
            .transpose()?,
    })
}

fn group(c: &Connection, id: i64) -> Result<Room, StoreError> {
    match room(c, id) {
        Ok(r) if r.name.is_some() => Ok(r),
        Ok(_) | Err(StoreError::NotFound { .. }) => Err(StoreError::NotFound {
            entity: "group",
            key: id.to_string(),
        }),
        Err(e) => Err(e),
    }
}

fn add_members(c: &Connection, id: i64, members: Vec<AgentId>) -> Result<(), StoreError> {
    let ts = fmt_ts(now());
    for agent in members {
        c.execute(
            "INSERT OR IGNORE INTO room_members (room_id, agent_id, joined_at)
             VALUES (?1, ?2, ?3)",
            params![id, agent.to_string(), ts],
        )?;
    }
    Ok(())
}

fn insert_room(
    c: &mut Connection,
    name: Option<&str>,
    members: Vec<AgentId>,
) -> Result<Room, StoreError> {
    let tx = c.transaction()?;
    tx.execute(
        "INSERT INTO rooms (created_at, name) VALUES (?1, ?2)",
        params![fmt_ts(now()), name],
    )?;
    let id = tx.last_insert_rowid();
    add_members(&tx, id, members)?;
    let created = room(&tx, id)?;
    tx.commit()?;
    Ok(created)
}

impl Store {
    /// Opens a call room with `members`.
    pub async fn create_room(&self, members: Vec<AgentId>) -> Result<Room, StoreError> {
        self.with(move |c| insert_room(c, None, members)).await
    }

    /// Creates group chat `name` with `members`.
    pub async fn create_group(
        &self,
        name: String,
        members: Vec<AgentId>,
    ) -> Result<Room, StoreError> {
        self.with(move |c| insert_room(c, Some(&name), members))
            .await
    }

    /// Group chats, oldest first.
    pub async fn groups(&self) -> Result<Vec<Room>, StoreError> {
        self.with(|c| {
            let ids: Vec<i64> = c
                .prepare("SELECT id FROM rooms WHERE name IS NOT NULL ORDER BY id")?
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            ids.into_iter().map(|id| room(c, id)).collect()
        })
        .await
    }

    /// One group chat; a call room is not found.
    pub async fn group(&self, id: i64) -> Result<Room, StoreError> {
        self.with(move |c| group(c, id)).await
    }

    /// Renames group `id`.
    pub async fn rename_group(&self, id: i64, name: String) -> Result<Room, StoreError> {
        self.with(move |c| {
            group(c, id)?;
            c.execute(
                "UPDATE rooms SET name = ?2 WHERE id = ?1",
                params![id, name],
            )?;
            room(c, id)
        })
        .await
    }

    /// Replaces group `id`'s members; those who stay keep their place.
    pub async fn set_group_members(
        &self,
        id: i64,
        members: Vec<AgentId>,
    ) -> Result<Room, StoreError> {
        self.with(move |c| {
            let tx = c.transaction()?;
            for old in group(&tx, id)?.members {
                if !members.contains(&old) {
                    tx.execute(
                        "DELETE FROM room_members WHERE room_id = ?1 AND agent_id = ?2",
                        params![id, old.to_string()],
                    )?;
                }
            }
            add_members(&tx, id, members)?;
            let changed = room(&tx, id)?;
            tx.commit()?;
            Ok(changed)
        })
        .await
    }

    /// Deletes group `id` with its lines and its members' turns in it.
    pub async fn delete_group(&self, id: i64) -> Result<(), StoreError> {
        self.with(move |c| {
            let tx = c.transaction()?;
            group(&tx, id)?;
            // Nulling `room_id` instead would move the turns into the members' chats.
            tx.execute("DELETE FROM messages WHERE room_id = ?1", [id])?;
            tx.execute("DELETE FROM rooms WHERE id = ?1", [id])?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Adds `agent` to the room unless it already took part.
    pub async fn add_room_member(&self, id: i64, agent: AgentId) -> Result<Room, StoreError> {
        self.with(move |c| {
            c.execute(
                "INSERT OR IGNORE INTO room_members (room_id, agent_id, joined_at)
                 VALUES (?1, ?2, ?3)",
                params![id, agent.to_string(), fmt_ts(now())],
            )?;
            room(c, id)
        })
        .await
    }

    /// Ends the call room (once); a group stays.
    pub async fn end_room(&self, id: i64) -> Result<Room, StoreError> {
        self.with(move |c| {
            c.execute(
                "UPDATE rooms SET ended_at = ?2
                 WHERE id = ?1 AND ended_at IS NULL AND name IS NULL",
                params![id, fmt_ts(now())],
            )?;
            room(c, id)
        })
        .await
    }

    /// Settles the room's queued turns so they never run; returns their message ids.
    pub async fn cancel_room_messages(&self, id: i64) -> Result<Vec<i64>, StoreError> {
        self.with(move |c| {
            let ids = c
                .prepare(
                    "UPDATE messages SET status = 'delivered', last_error = 'the call ended',
                                         updated_at = ?2
                     WHERE room_id = ?1 AND status = 'pending' RETURNING id",
                )?
                .query_map(params![id, fmt_ts(now())], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            Ok(ids)
        })
        .await
    }

    /// Ends every running call room: calls live in the daemon's memory, so a restart ends them.
    pub async fn end_open_rooms(&self) -> Result<usize, StoreError> {
        self.with(|c| {
            Ok(c.execute(
                "UPDATE rooms SET ended_at = ?1 WHERE ended_at IS NULL AND name IS NULL",
                [fmt_ts(now())],
            )?)
        })
        .await
    }

    /// Stores something said in the room, or with `invited` the topic of an invitation.
    pub async fn add_room_line(
        &self,
        id: i64,
        speaker: Sender,
        body: String,
        invited: Option<AgentId>,
    ) -> Result<RoomLine, StoreError> {
        self.with(move |c| {
            c.query_row(
                "INSERT INTO room_lines (room_id, speaker, body, created_at, invited)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 RETURNING id, room_id, speaker, body, created_at, invited",
                params![
                    id,
                    speaker.to_string(),
                    body,
                    fmt_ts(now()),
                    invited.map(|a| a.to_string())
                ],
                |row| Ok(line(row)),
            )?
        })
        .await
    }

    /// The newest call rooms first; groups are left out.
    pub async fn rooms(&self, limit: u32) -> Result<Vec<Room>, StoreError> {
        self.with(move |c| {
            let ids: Vec<i64> = c
                .prepare("SELECT id FROM rooms WHERE name IS NULL ORDER BY id DESC LIMIT ?1")?
                .query_map([limit], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            ids.into_iter().map(|id| room(c, id)).collect()
        })
        .await
    }

    /// One room (a call room or a group) with its members.
    pub async fn room(&self, id: i64) -> Result<Room, StoreError> {
        self.with(move |c| room(c, id)).await
    }

    /// The room's last `limit` lines, oldest first.
    pub async fn room_lines(&self, id: i64, limit: u32) -> Result<Vec<RoomLine>, StoreError> {
        self.with(move |c| {
            let mut stmt = c.prepare(
                "SELECT * FROM (SELECT id, room_id, speaker, body, created_at, invited FROM room_lines
                 WHERE room_id = ?1 ORDER BY id DESC LIMIT ?2) ORDER BY id",
            )?;
            let mut rows = stmt.query(params![id, limit])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(line(row)?);
            }
            Ok(out)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use ferrobot_core::{AgentName, MailStatus};

    use super::*;
    use crate::NewAgent;

    #[tokio::test]
    async fn rooms_keep_members_lines_and_their_turns_out_of_the_chat() {
        let store = Store::open_in_memory().unwrap();
        let mut ids = Vec::new();
        for n in ["ann", "bob", "cy"] {
            let a = store
                .create_agent(NewAgent {
                    created_by: None,
                    name: AgentName::parse(n).unwrap(),
                    description: String::new(),
                    instructions: String::new(),
                })
                .await
                .unwrap();
            ids.push(a.id);
        }
        let [ann, bob, cy] = ids[..] else {
            unreachable!()
        };
        let r = store.create_room(vec![ann, bob]).await.unwrap();
        assert_eq!((r.members.as_slice(), r.ended_at), (&[ann, bob][..], None));
        store.add_room_member(r.id, cy).await.unwrap();
        let r = store.add_room_member(r.id, ann).await.unwrap();
        assert_eq!(r.members, [ann, bob, cy], "joined order, once each");
        store
            .add_room_line(r.id, Sender::User, "hi all".into(), None)
            .await
            .unwrap();
        let said = store
            .add_room_line(r.id, Sender::Agent(bob), "hi".into(), Some(cy))
            .await
            .unwrap();
        assert_eq!(
            (said.speaker, said.body.as_str(), said.invited),
            (Sender::Agent(bob), "hi", Some(cy))
        );
        let lines = store.room_lines(r.id, 1).await.unwrap();
        assert_eq!(lines, [said], "the newest within the limit");

        let turn = store
            .enqueue_room_message(Sender::Agent(bob), ann, "hi".into(), r.id)
            .await
            .unwrap();
        assert_eq!(
            (turn.room_id, turn.priority),
            (Some(r.id), Some(true)),
            "goes first"
        );
        store
            .enqueue_message(Sender::Agent(bob), ann, "mail".into(), None)
            .await
            .unwrap();
        let chat: Vec<_> = store.conversation(ann, 10).await.unwrap();
        assert_eq!(chat.len(), 1, "room turns stay out of the chat");
        assert_eq!(store.pair_thread(ann, bob, 10).await.unwrap().len(), 1);
        assert_eq!(store.agent_pairs().await.unwrap()[0].messages, 1);

        // Steering takes the operator's words of the running turn's room only.
        let op = store
            .enqueue_room_message(Sender::User, cy, "go".into(), r.id)
            .await
            .unwrap();
        assert_eq!(store.claim_next_user_message(cy, None).await.unwrap(), None);
        let claimed = store.claim_next_user_message(cy, Some(r.id)).await.unwrap();
        assert_eq!(
            claimed.map(|m| (m.id, m.status)),
            Some((op.id, MailStatus::InFlight))
        );

        assert_eq!(
            store.cancel_room_messages(r.id).await.unwrap(),
            [turn.id],
            "queued turns only; the running one ends by itself"
        );
        let next = store.claim_next_message(ann).await.unwrap();
        assert_eq!(
            next.map(|m| m.body).as_deref(),
            Some("mail"),
            "chat mail stays"
        );
        assert_eq!(store.end_open_rooms().await.unwrap(), 1);
        let ended = store.end_room(r.id).await.unwrap();
        assert!(ended.ended_at.is_some());
        assert_eq!(store.rooms(10).await.unwrap(), [ended]);
        assert!(matches!(
            store.room(r.id + 1).await,
            Err(StoreError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn groups_are_named_rooms_kept_apart_from_calls() {
        let store = Store::open_in_memory().unwrap();
        let mut ids = Vec::new();
        for n in ["ann", "bob", "cy"] {
            let a = store
                .create_agent(NewAgent {
                    created_by: None,
                    name: AgentName::parse(n).unwrap(),
                    description: String::new(),
                    instructions: String::new(),
                })
                .await
                .unwrap();
            ids.push(a.id);
        }
        let [ann, bob, cy] = ids[..] else {
            unreachable!()
        };
        let call = store.create_room(vec![ann, bob]).await.unwrap();
        let g = store
            .create_group("Ops".into(), vec![ann, bob])
            .await
            .unwrap();
        assert_eq!(
            (g.name.as_deref(), g.members.as_slice()),
            (Some("Ops"), &[ann, bob][..])
        );
        assert_eq!(store.groups().await.unwrap(), std::slice::from_ref(&g));
        assert_eq!(
            store.rooms(10).await.unwrap(),
            std::slice::from_ref(&call),
            "calls only"
        );
        assert!(matches!(
            store.group(call.id).await,
            Err(StoreError::NotFound {
                entity: "group",
                ..
            })
        ));

        // Ending calls never ends a group.
        assert_eq!(store.end_open_rooms().await.unwrap(), 1);
        assert_eq!(store.end_room(g.id).await.unwrap().ended_at, None);

        let g = store.rename_group(g.id, "Ops team".into()).await.unwrap();
        assert_eq!(g.name.as_deref(), Some("Ops team"));
        assert!(store.rename_group(call.id, "x".into()).await.is_err());
        let g = store.set_group_members(g.id, vec![cy, bob]).await.unwrap();
        assert_eq!(
            g.members,
            [bob, cy],
            "bob keeps its place, cy joins, ann left"
        );

        store
            .add_room_line(g.id, Sender::User, "hi".into(), None)
            .await
            .unwrap();
        let turn = store
            .enqueue_room_message(Sender::User, bob, "hi".into(), g.id)
            .await
            .unwrap();
        store.delete_group(g.id).await.unwrap();
        assert_eq!(store.groups().await.unwrap(), []);
        assert!(
            store.get_message(turn.id).await.is_err(),
            "its turns go too"
        );
        assert_eq!(store.conversation(bob, 10).await.unwrap(), []);
        assert!(store.delete_group(call.id).await.is_err(), "not a group");
        assert!(store.room(call.id).await.is_ok());
    }
}
