//! Call rooms: a voice call with two or more agents, who took part and what was said.

use ferrobot_core::{AgentId, Sender};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use time::OffsetDateTime;

use crate::util::{fmt_ts, now, parse_ts};
use crate::{Store, StoreError};

/// A voice call with two or more agents; kept as history once the call ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct Room {
    pub id: i64,
    /// Every agent that took part, in the order they joined.
    pub members: Vec<AgentId>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
    /// When the call ended; `null` while it runs.
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
    /// The words; mentions are `<@agent-id>` tokens.
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

fn room(c: &Connection, id: i64) -> Result<Room, StoreError> {
    let row = c
        .query_row(
            "SELECT created_at, ended_at FROM rooms WHERE id = ?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    let Some((created, ended)) = row else {
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
    })
}

impl Store {
    /// Opens a room with `members`.
    pub async fn create_room(&self, members: Vec<AgentId>) -> Result<Room, StoreError> {
        self.with(move |c| {
            let tx = c.transaction()?;
            let ts = fmt_ts(now());
            tx.execute("INSERT INTO rooms (created_at) VALUES (?1)", [&ts])?;
            let id = tx.last_insert_rowid();
            for agent in members {
                tx.execute(
                    "INSERT OR IGNORE INTO room_members (room_id, agent_id, joined_at)
                     VALUES (?1, ?2, ?3)",
                    params![id, agent.to_string(), ts],
                )?;
            }
            let created = room(&tx, id)?;
            tx.commit()?;
            Ok(created)
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

    /// Ends the room (once).
    pub async fn end_room(&self, id: i64) -> Result<Room, StoreError> {
        self.with(move |c| {
            c.execute(
                "UPDATE rooms SET ended_at = ?2 WHERE id = ?1 AND ended_at IS NULL",
                params![id, fmt_ts(now())],
            )?;
            room(c, id)
        })
        .await
    }

    /// Ends every running room: calls live in the daemon's memory, so a restart ends them.
    pub async fn end_open_rooms(&self) -> Result<usize, StoreError> {
        self.with(|c| {
            Ok(c.execute(
                "UPDATE rooms SET ended_at = ?1 WHERE ended_at IS NULL",
                [fmt_ts(now())],
            )?)
        })
        .await
    }

    /// Stores something said in the room.
    pub async fn add_room_line(
        &self,
        id: i64,
        speaker: Sender,
        body: String,
    ) -> Result<RoomLine, StoreError> {
        self.with(move |c| {
            c.query_row(
                "INSERT INTO room_lines (room_id, speaker, body, created_at)
                 VALUES (?1, ?2, ?3, ?4)
                 RETURNING id, room_id, speaker, body, created_at",
                params![id, speaker.to_string(), body, fmt_ts(now())],
                |row| Ok(line(row)),
            )?
        })
        .await
    }

    /// The newest rooms first.
    pub async fn rooms(&self, limit: u32) -> Result<Vec<Room>, StoreError> {
        self.with(move |c| {
            let ids: Vec<i64> = c
                .prepare("SELECT id FROM rooms ORDER BY id DESC LIMIT ?1")?
                .query_map([limit], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            ids.into_iter().map(|id| room(c, id)).collect()
        })
        .await
    }

    /// One room with its members.
    pub async fn room(&self, id: i64) -> Result<Room, StoreError> {
        self.with(move |c| room(c, id)).await
    }

    /// The room's last `limit` lines, oldest first.
    pub async fn room_lines(&self, id: i64, limit: u32) -> Result<Vec<RoomLine>, StoreError> {
        self.with(move |c| {
            let mut stmt = c.prepare(
                "SELECT * FROM (SELECT id, room_id, speaker, body, created_at FROM room_lines
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
            .add_room_line(r.id, Sender::User, "hi all".into())
            .await
            .unwrap();
        let said = store
            .add_room_line(r.id, Sender::Agent(bob), "hi".into())
            .await
            .unwrap();
        assert_eq!(
            (said.speaker, said.body.as_str()),
            (Sender::Agent(bob), "hi")
        );
        let lines = store.room_lines(r.id, 1).await.unwrap();
        assert_eq!(lines, [said], "the newest within the limit");

        let turn = store
            .enqueue_room_message(Sender::Agent(bob), ann, "hi".into(), r.id)
            .await
            .unwrap();
        assert_eq!(turn.room_id, Some(r.id));
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

        assert_eq!(store.end_open_rooms().await.unwrap(), 1);
        let ended = store.end_room(r.id).await.unwrap();
        assert!(ended.ended_at.is_some());
        assert_eq!(store.rooms(10).await.unwrap(), [ended]);
        assert!(matches!(
            store.room(r.id + 1).await,
            Err(StoreError::NotFound { .. })
        ));
    }
}
