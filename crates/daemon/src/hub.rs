//! Tracks connected runners and wakes their dispatch loops.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use ferrobot_core::AgentId;
use ferrobot_core::protocol::DaemonFrame;
use tokio::sync::{Notify, mpsc};

#[derive(Debug)]
struct Conn {
    id: u64,
    out: mpsc::Sender<DaemonFrame>,
    current_turn: Option<String>,
}

/// Registry of live runner connections plus one wake-up signal per agent.
#[derive(Debug, Clone, Default)]
pub struct Hub {
    inner: Arc<Mutex<Inner>>,
    next_id: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
struct Inner {
    conns: HashMap<AgentId, Conn>,
    notifies: HashMap<AgentId, Arc<Notify>>,
}

impl Hub {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock only means another task panicked mid-update of a
        // plain map; the data is still consistent enough to keep serving.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The wake-up signal for an agent's dispatch loop.
    pub fn notify_handle(&self, agent: AgentId) -> Arc<Notify> {
        Arc::clone(self.lock().notifies.entry(agent).or_default())
    }

    /// Registers a connection, replacing (and thereby closing) an older one.
    /// Returns the connection id used for unregistering.
    pub fn register(&self, agent: AgentId, out: mpsc::Sender<DaemonFrame>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.lock().conns.insert(
            agent,
            Conn {
                id,
                out,
                current_turn: None,
            },
        );
        id
    }

    /// Removes the connection if it is still the registered one.
    pub fn unregister(&self, agent: AgentId, conn_id: u64) {
        let mut inner = self.lock();
        if inner.conns.get(&agent).is_some_and(|c| c.id == conn_id) {
            inner.conns.remove(&agent);
        }
    }

    pub fn is_connected(&self, agent: AgentId) -> bool {
        self.lock().conns.contains_key(&agent)
    }

    pub fn set_current_turn(&self, agent: AgentId, conn_id: u64, turn: Option<String>) {
        if let Some(c) = self
            .lock()
            .conns
            .get_mut(&agent)
            .filter(|c| c.id == conn_id)
        {
            c.current_turn = turn;
        }
    }

    /// Asks the runner to abort its current turn. Returns the turn id, if any.
    pub fn interrupt(&self, agent: AgentId) -> Option<String> {
        let inner = self.lock();
        let conn = inner.conns.get(&agent)?;
        let turn_id = conn.current_turn.clone()?;
        // Best effort: a full queue means the runner is gone anyway.
        let _ = conn.out.try_send(DaemonFrame::Interrupt {
            turn_id: turn_id.clone(),
        });
        Some(turn_id)
    }

    pub fn forget(&self, agent: AgentId) {
        let mut inner = self.lock();
        inner.conns.remove(&agent);
        inner.notifies.remove(&agent);
    }

    /// Wakes the agent's dispatch loop (no-op if nothing is waiting yet; the
    /// stored permit makes the next wait return immediately).
    pub fn wake(&self, agent: AgentId) {
        self.notify_handle(agent).notify_one();
    }
}

impl ferrobot_mcp::MailboxNotifier for Hub {
    fn message_enqueued(&self, recipient: AgentId, _message_id: i64) {
        self.wake(recipient);
    }
}
