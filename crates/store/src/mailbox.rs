//! Durable agent-to-agent mailbox with at-least-once delivery.

use ferrobot_core::{AgentId, MailMessage, MailStatus, Sender};
use rusqlite::{OptionalExtension, Row, params};

use crate::util::{fmt_ts, now, parse_ts};
use crate::{MAX_DELIVERY_ATTEMPTS, Store, StoreError};

const COLUMNS: &str = "id, sender, recipient, body, reply_to, status, attempts, created_at";

fn from_row(row: &Row<'_>) -> Result<MailMessage, StoreError> {
    let sender: String = row.get(1)?;
    let recipient: String = row.get(2)?;
    let status: String = row.get(5)?;
    let created: String = row.get(7)?;
    Ok(MailMessage {
        id: row.get(0)?,
        sender: sender.parse()?,
        recipient: recipient.parse()?,
        body: row.get(3)?,
        reply_to: row.get(4)?,
        status: status.parse()?,
        attempts: row.get(6)?,
        created_at: parse_ts(&created)?,
    })
}

impl Store {
    /// Appends a message to `recipient`'s queue.
    pub async fn enqueue_message(
        &self,
        sender: Sender,
        recipient: AgentId,
        body: String,
        reply_to: Option<i64>,
    ) -> Result<MailMessage, StoreError> {
        self.with(move |c| {
            let ts = fmt_ts(now());
            c.execute(
                "INSERT INTO messages (sender, recipient, body, reply_to, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?5)",
                params![sender.to_string(), recipient.to_string(), body, reply_to, ts],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _)
                    if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY =>
                {
                    StoreError::NotFound {
                        entity: "agent or reply_to message",
                        key: recipient.to_string(),
                    }
                }
                other => other.into(),
            })?;
            let id = c.last_insert_rowid();
            c.query_row(
                &format!("SELECT {COLUMNS} FROM messages WHERE id = ?1"),
                [id],
                |row| Ok(from_row(row)),
            )?
        })
        .await
    }

    /// Fetches one message by id.
    pub async fn get_message(&self, id: i64) -> Result<MailMessage, StoreError> {
        self.with(move |c| {
            c.query_row(
                &format!("SELECT {COLUMNS} FROM messages WHERE id = ?1"),
                [id],
                |row| Ok(from_row(row)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound {
                entity: "message",
                key: id.to_string(),
            })?
        })
        .await
    }

    /// Atomically claims the oldest pending message for `agent`, marking it in flight.
    pub async fn claim_next_message(
        &self,
        agent: AgentId,
    ) -> Result<Option<MailMessage>, StoreError> {
        self.with(move |c| {
            let tx = c.transaction()?;
            let claimed = tx
                .query_row(
                    &format!(
                        "UPDATE messages SET status = 'in_flight', attempts = attempts + 1, updated_at = ?2
                         WHERE id = (SELECT id FROM messages WHERE recipient = ?1 AND status = 'pending'
                                     ORDER BY id LIMIT 1)
                         RETURNING {COLUMNS}"
                    ),
                    params![agent.to_string(), fmt_ts(now())],
                    |row| Ok(from_row(row)),
                )
                .optional()?
                .transpose()?;
            tx.commit()?;
            Ok(claimed)
        })
        .await
    }

    /// Marks an in-flight message delivered.
    pub async fn complete_message(&self, id: i64) -> Result<(), StoreError> {
        self.with(move |c| {
            c.execute(
                "UPDATE messages SET status = 'delivered', updated_at = ?2 WHERE id = ?1",
                params![id, fmt_ts(now())],
            )?;
            Ok(())
        })
        .await
    }

    /// Records a failed delivery. Requeues unless attempts are exhausted.
    /// Returns the resulting status.
    pub async fn fail_message(&self, id: i64, error: String) -> Result<MailStatus, StoreError> {
        self.with(move |c| {
            let status: String = c.query_row(
                "UPDATE messages
                 SET status = CASE WHEN attempts >= ?3 THEN 'failed' ELSE 'pending' END,
                     last_error = ?2, updated_at = ?4
                 WHERE id = ?1 RETURNING status",
                params![id, error, MAX_DELIVERY_ATTEMPTS, fmt_ts(now())],
                |r| r.get(0),
            )?;
            Ok(status.parse()?)
        })
        .await
    }

    /// Puts all in-flight messages of `agent` back into the queue.
    /// Called when a runner (re)connects or disconnects mid-turn.
    pub async fn requeue_in_flight(&self, agent: AgentId) -> Result<usize, StoreError> {
        self.with(move |c| {
            Ok(c.execute(
                "UPDATE messages SET status = 'pending', updated_at = ?2
                 WHERE recipient = ?1 AND status = 'in_flight'",
                params![agent.to_string(), fmt_ts(now())],
            )?)
        })
        .await
    }

    /// Most recent messages addressed to `agent` (newest last).
    pub async fn inbox(&self, agent: AgentId, limit: u32) -> Result<Vec<MailMessage>, StoreError> {
        self.with(move |c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUMNS} FROM (SELECT * FROM messages WHERE recipient = ?1
                 ORDER BY id DESC LIMIT ?2) ORDER BY id"
            ))?;
            let mut rows = stmt.query(params![agent.to_string(), limit])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(from_row(row)?);
            }
            Ok(out)
        })
        .await
    }

    /// All messages sent to or by `agent` (newest last), for the UI.
    pub async fn conversation(
        &self,
        agent: AgentId,
        limit: u32,
    ) -> Result<Vec<MailMessage>, StoreError> {
        self.with(move |c| {
            let id = agent.to_string();
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUMNS} FROM (SELECT * FROM messages WHERE recipient = ?1 OR sender = ?2
                 ORDER BY id DESC LIMIT ?3) ORDER BY id"
            ))?;
            let mut rows = stmt.query(params![id, format!("agent:{id}"), limit])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(from_row(row)?);
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

    async fn setup() -> (Store, AgentId, AgentId) {
        let store = Store::open_in_memory().unwrap();
        let mut ids = Vec::new();
        for n in ["alice", "bob"] {
            let a = store
                .create_agent(NewAgent {
                    name: AgentName::parse(n).unwrap(),
                    instructions: String::new(),
                })
                .await
                .unwrap();
            ids.push(a.id);
        }
        (store, ids[0], ids[1])
    }

    #[tokio::test]
    async fn fifo_claim_and_complete() {
        let (store, alice, bob) = setup().await;
        let m1 = store
            .enqueue_message(Sender::Agent(alice), bob, "one".into(), None)
            .await
            .unwrap();
        store
            .enqueue_message(Sender::User, bob, "two".into(), Some(m1.id))
            .await
            .unwrap();

        let c1 = store.claim_next_message(bob).await.unwrap().unwrap();
        assert_eq!(
            (c1.body.as_str(), c1.status, c1.attempts),
            ("one", MailStatus::InFlight, 1)
        );
        let c2 = store.claim_next_message(bob).await.unwrap().unwrap();
        assert_eq!(c2.body, "two");
        assert_eq!(c2.reply_to, Some(m1.id));
        assert!(store.claim_next_message(bob).await.unwrap().is_none());
        assert!(store.claim_next_message(alice).await.unwrap().is_none());

        store.complete_message(c1.id).await.unwrap();
        let inbox = store.inbox(bob, 10).await.unwrap();
        assert_eq!(inbox[0].status, MailStatus::Delivered);
        assert_eq!(inbox[1].status, MailStatus::InFlight);
    }

    #[tokio::test]
    async fn requeue_after_disconnect() {
        let (store, _alice, bob) = setup().await;
        store
            .enqueue_message(Sender::User, bob, "hi".into(), None)
            .await
            .unwrap();
        store.claim_next_message(bob).await.unwrap().unwrap();
        assert_eq!(store.requeue_in_flight(bob).await.unwrap(), 1);
        let again = store.claim_next_message(bob).await.unwrap().unwrap();
        assert_eq!(again.attempts, 2);
    }

    #[tokio::test]
    async fn fails_after_max_attempts() {
        let (store, _alice, bob) = setup().await;
        let m = store
            .enqueue_message(Sender::User, bob, "x".into(), None)
            .await
            .unwrap();
        for attempt in 1..=MAX_DELIVERY_ATTEMPTS {
            let claimed = store.claim_next_message(bob).await.unwrap().unwrap();
            assert_eq!(claimed.id, m.id);
            let status = store.fail_message(m.id, "boom".into()).await.unwrap();
            let expected = if attempt == MAX_DELIVERY_ATTEMPTS {
                MailStatus::Failed
            } else {
                MailStatus::Pending
            };
            assert_eq!(status, expected);
        }
        assert!(store.claim_next_message(bob).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn unknown_recipient_is_not_found() {
        let (store, _, _) = setup().await;
        let err = store
            .enqueue_message(Sender::User, AgentId::new(), "x".into(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::NotFound { .. }));
    }

    #[tokio::test]
    async fn conversation_includes_sent_and_received() {
        let (store, alice, bob) = setup().await;
        store
            .enqueue_message(Sender::User, alice, "to alice".into(), None)
            .await
            .unwrap();
        store
            .enqueue_message(Sender::Agent(alice), bob, "to bob".into(), None)
            .await
            .unwrap();
        store
            .enqueue_message(Sender::User, bob, "unrelated".into(), None)
            .await
            .unwrap();
        let conv = store.conversation(alice, 10).await.unwrap();
        let bodies: Vec<_> = conv.iter().map(|m| m.body.as_str()).collect();
        assert_eq!(bodies, ["to alice", "to bob"]);
    }
}
