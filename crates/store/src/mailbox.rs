//! Durable agent-to-agent mailbox with at-least-once delivery.

use ferrobot_core::{AgentId, MailMessage, MailStatus, Sender};
use rusqlite::{OptionalExtension, Row, params};

use crate::util::{fmt_ts, now, parse_ts};
use crate::{MAX_DELIVERY_ATTEMPTS, Store, StoreError};

const COLUMNS: &str =
    "id, sender, recipient, body, reply_to, status, attempts, created_at, priority";

fn priority_to_db(p: Option<bool>) -> i64 {
    match p {
        Some(true) => 1,
        None => 0,
        Some(false) => -1,
    }
}

/// Messages between two agents (either direction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPair {
    /// The pair, ordered by id.
    pub a: AgentId,
    pub b: AgentId,
    pub messages: u32,
    pub last: MailMessage,
}

fn from_row(row: &Row<'_>) -> Result<MailMessage, StoreError> {
    from_row_at(row, 0)
}

/// Reads [`COLUMNS`] starting at column `o`.
fn from_row_at(row: &Row<'_>, o: usize) -> Result<MailMessage, StoreError> {
    let sender: String = row.get(o + 1)?;
    let recipient: String = row.get(o + 2)?;
    let status: String = row.get(o + 5)?;
    let created: String = row.get(o + 7)?;
    Ok(MailMessage {
        id: row.get(o)?,
        sender: sender.parse()?,
        recipient: recipient.parse()?,
        body: row.get(o + 3)?,
        reply_to: row.get(o + 4)?,
        status: status.parse()?,
        attempts: row.get(o + 6)?,
        created_at: parse_ts(&created)?,
        priority: match row.get::<_, i64>(o + 8)? {
            0 => None,
            p => Some(p > 0),
        },
    })
}

/// Inserts a pending message (shared with routine firing, which needs it
/// inside its own transaction).
pub(crate) fn insert_message(
    c: &rusqlite::Connection,
    sender: Sender,
    recipient: AgentId,
    body: &str,
    reply_to: Option<i64>,
    priority: Option<bool>,
) -> Result<MailMessage, StoreError> {
    let ts = fmt_ts(now());
    c.execute(
        "INSERT INTO messages (sender, recipient, body, reply_to, status, created_at, updated_at,
                               priority)
         VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?5, ?6)",
        params![
            sender.to_string(),
            recipient.to_string(),
            body,
            reply_to,
            ts,
            priority_to_db(priority)
        ],
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
        self.enqueue_message_with_priority(sender, recipient, body, reply_to, None)
            .await
    }

    /// Like [`Store::enqueue_message`], with a priority (see `MailMessage::priority`).
    pub async fn enqueue_message_with_priority(
        &self,
        sender: Sender,
        recipient: AgentId,
        body: String,
        reply_to: Option<i64>,
        priority: Option<bool>,
    ) -> Result<MailMessage, StoreError> {
        self.with(move |c| insert_message(c, sender, recipient, &body, reply_to, priority))
            .await
    }

    /// True if the agent has pending or in-flight messages.
    pub async fn has_queued_messages(&self, agent: AgentId) -> Result<bool, StoreError> {
        self.queued(agent, false).await
    }

    /// True if the agent has queued messages that should wake it (all but
    /// `priority: false` ones).
    pub async fn has_waking_messages(&self, agent: AgentId) -> Result<bool, StoreError> {
        self.queued(agent, true).await
    }

    async fn queued(&self, agent: AgentId, waking_only: bool) -> Result<bool, StoreError> {
        self.with(move |c| {
            Ok(c.query_row(
                "SELECT EXISTS (SELECT 1 FROM messages
                 WHERE recipient = ?1 AND status IN ('pending', 'in_flight')
                   AND (?2 = 0 OR priority >= 0))",
                params![agent.to_string(), waking_only],
                |r| r.get(0),
            )?)
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

    /// Atomically claims the next pending message for `agent` (urgent ones
    /// first, then oldest first), marking it in flight.
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
                                     ORDER BY priority > 0 DESC, id LIMIT 1)
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

    /// Every pair of agents that exchanged messages, most recent first.
    pub async fn agent_pairs(&self) -> Result<Vec<AgentPair>, StoreError> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "WITH m AS (
                    SELECT *, substr(sender, 7) AS from_id FROM messages
                    WHERE sender LIKE 'agent:%'
                 ), p AS (
                    SELECT min(from_id, recipient) AS a, max(from_id, recipient) AS b,
                           count(*) AS n, max(id) AS last_id
                    FROM m GROUP BY 1, 2
                 )
                 SELECT p.a, p.b, p.n, {} FROM p JOIN messages ON messages.id = p.last_id
                 ORDER BY p.last_id DESC",
                COLUMNS
                    .split(", ")
                    .map(|c| format!("messages.{c}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))?;
            let mut rows = stmt.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let a: String = row.get(0)?;
                let b: String = row.get(1)?;
                let last = from_row_at(row, 3)?;
                out.push(AgentPair {
                    a: a.parse()?,
                    b: b.parse()?,
                    messages: row.get(2)?,
                    last,
                });
            }
            Ok(out)
        })
        .await
    }

    /// Messages between two agents in both directions (newest last).
    pub async fn pair_thread(
        &self,
        a: AgentId,
        b: AgentId,
        limit: u32,
    ) -> Result<Vec<MailMessage>, StoreError> {
        self.with(move |c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUMNS} FROM (SELECT * FROM messages
                 WHERE (sender = ?1 AND recipient = ?4) OR (sender = ?3 AND recipient = ?2)
                 ORDER BY id DESC LIMIT ?5) ORDER BY id"
            ))?;
            let mut rows = stmt.query(params![
                format!("agent:{a}"),
                a.to_string(),
                format!("agent:{b}"),
                b.to_string(),
                limit
            ])?;
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
                    description: String::new(),
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

    #[tokio::test]
    async fn priority_orders_and_deferred_does_not_wake() {
        let (store, a, b) = setup().await;
        let send = |body: &str, p: Option<bool>| {
            store.enqueue_message_with_priority(Sender::Agent(a), b, body.into(), None, p)
        };
        send("later", Some(false)).await.unwrap();
        assert!(store.has_queued_messages(b).await.unwrap());
        assert!(!store.has_waking_messages(b).await.unwrap());
        send("normal", None).await.unwrap();
        let urgent = send("urgent", Some(true)).await.unwrap();
        assert_eq!(urgent.priority, Some(true));
        assert!(store.has_waking_messages(b).await.unwrap());
        let mut order = Vec::new();
        while let Some(m) = store.claim_next_message(b).await.unwrap() {
            order.push(m.body.clone());
            store.complete_message(m.id).await.unwrap();
        }
        assert_eq!(order, ["urgent", "later", "normal"]);
    }

    #[tokio::test]
    async fn pairs_and_threads() {
        let (store, a, b) = setup().await;
        store
            .enqueue_message(Sender::User, a, "hi".into(), None)
            .await
            .unwrap();
        let first = store
            .enqueue_message(Sender::Agent(a), b, "ping".into(), None)
            .await
            .unwrap();
        store
            .enqueue_message(Sender::Agent(b), a, "pong".into(), Some(first.id))
            .await
            .unwrap();
        let pairs = store.agent_pairs().await.unwrap();
        assert_eq!(pairs.len(), 1);
        let p = &pairs[0];
        assert_eq!((p.a.min(p.b), p.messages), (a.min(b), 2));
        assert_eq!(p.last.body, "pong");
        let bodies: Vec<_> = store
            .pair_thread(b, a, 50)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.body)
            .collect();
        assert_eq!(bodies, ["ping", "pong"]);
    }
}
