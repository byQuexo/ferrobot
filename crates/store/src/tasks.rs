//! Tracked tasks: messages handed over with `assign_task` or `create_agent`, marked by a `task`
//! event on the delegate; their state is derived from the mailbox, with no table of their own.

use ferrobot_core::{AgentId, MailMessage, MailStatus, Sender};
use rusqlite::{OptionalExtension, Row, params};

use crate::mailbox::{COLUMNS, from_row_at, insert_message};
use crate::util::{fmt_ts, now};
use crate::{Store, StoreError};

/// A task and how far it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// The task message: its sender assigned it, its recipient works on it.
    pub message: MailMessage,
    /// The delegate answered in the task's thread (itself or automatically).
    pub reported: bool,
    /// The supervisor already sent its one reminder.
    pub nudged: bool,
}

impl Task {
    pub fn assigner(&self) -> Option<AgentId> {
        match self.message.sender {
            Sender::Agent(id) => Some(id),
            _ => None,
        }
    }

    /// `queued`, `working`, `done`, `open` (finished without an answer) or `failed`.
    pub fn status(&self) -> &'static str {
        match self.message.status {
            MailStatus::Pending => "queued",
            MailStatus::InFlight => "working",
            MailStatus::Failed => "failed",
            MailStatus::Delivered if self.reported => "done",
            MailStatus::Delivered => "open",
        }
    }
}

/// Tasks with their state; `filter` continues the `WHERE`.
fn select(filter: &str) -> String {
    let columns: Vec<String> = COLUMNS.split(", ").map(|c| format!("m.{c}")).collect();
    format!(
        "SELECT {},
           EXISTS (SELECT 1 FROM messages r LEFT JOIN messages p ON p.id = r.reply_to
                   WHERE r.sender = 'agent:' || m.recipient AND r.recipient = substr(m.sender, 7)
                     AND (r.reply_to = m.id OR p.reply_to = m.id)),
           EXISTS (SELECT 1 FROM events n WHERE n.agent_id = m.recipient
                     AND n.kind = 'task_nudged' AND json_extract(n.payload, '$.message_id') = m.id)
         FROM events e JOIN messages m ON m.id = json_extract(e.payload, '$.message_id')
         WHERE e.kind = 'task' AND e.agent_id = m.recipient {filter}",
        columns.join(", ")
    )
}

fn task_row(row: &Row<'_>) -> Result<Task, StoreError> {
    let n = COLUMNS.split(", ").count();
    Ok(Task {
        message: from_row_at(row, 0)?,
        reported: row.get(n)?,
        nudged: row.get(n + 1)?,
    })
}

fn mark(
    c: &rusqlite::Connection,
    agent: AgentId,
    kind: &str,
    task: i64,
) -> rusqlite::Result<usize> {
    c.execute(
        "INSERT INTO events (agent_id, turn_id, kind, payload, created_at)
         VALUES (?1, NULL, ?2, ?3, ?4)",
        params![
            agent.to_string(),
            kind,
            serde_json::json!({ "message_id": task }).to_string(),
            fmt_ts(now())
        ],
    )
}

impl Store {
    /// Queues `body` from agent `from` to `to` as a task.
    pub async fn enqueue_task(
        &self,
        from: AgentId,
        to: AgentId,
        body: String,
    ) -> Result<MailMessage, StoreError> {
        self.with(move |c| {
            let tx = c.transaction()?;
            let msg = insert_message(&tx, Sender::Agent(from), to, &body, None, None)?;
            mark(&tx, to, "task", msg.id)?;
            tx.commit()?;
            Ok(msg)
        })
        .await
    }

    /// The task `agent`'s turn on `msg` works on: `msg` itself, or the task a
    /// follow-up from its assigner is threaded to (e.g. the reminder).
    pub async fn task_of(
        &self,
        agent: AgentId,
        msg: &MailMessage,
    ) -> Result<Option<Task>, StoreError> {
        let (id, reply_to, sender) = (msg.id, msg.reply_to, msg.sender);
        self.with(move |c| {
            let task = c
                .query_row(
                    &select("AND m.recipient = ?1 AND m.id IN (?2, ?3)"),
                    params![agent.to_string(), id, reply_to],
                    |row| Ok(task_row(row)),
                )
                .optional()?
                .transpose()?;
            Ok(task.filter(|t| t.message.id == id || t.message.sender == sender))
        })
        .await
    }

    /// Tasks `agent` gave or received, newest first.
    pub async fn tasks(&self, agent: AgentId, limit: u32) -> Result<Vec<Task>, StoreError> {
        self.with(move |c| {
            let mut stmt = c.prepare(&select(
                "AND (m.recipient = ?1 OR m.sender = ?2) ORDER BY m.id DESC LIMIT ?3",
            ))?;
            let mut rows =
                stmt.query(params![agent.to_string(), format!("agent:{agent}"), limit])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(task_row(row)?);
            }
            Ok(out)
        })
        .await
    }

    /// Finished tasks with neither an answer nor a reminder yet.
    pub async fn unanswered_tasks(&self) -> Result<Vec<Task>, StoreError> {
        self.with(|c| {
            let mut stmt = c.prepare(&select("AND m.status = 'delivered'"))?;
            let mut rows = stmt.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let task = task_row(row)?;
                if !task.reported && !task.nudged {
                    out.push(task);
                }
            }
            Ok(out)
        })
        .await
    }

    /// Queues the one reminder for `task`, from its assigner and threaded to it.
    pub async fn nudge_task(&self, task: &Task, body: String) -> Result<MailMessage, StoreError> {
        let (id, sender, delegate) = (task.message.id, task.message.sender, task.message.recipient);
        self.with(move |c| {
            let tx = c.transaction()?;
            let msg = insert_message(&tx, sender, delegate, &body, Some(id), None)?;
            mark(&tx, delegate, "task_nudged", id)?;
            tx.commit()?;
            Ok(msg)
        })
        .await
    }
}
