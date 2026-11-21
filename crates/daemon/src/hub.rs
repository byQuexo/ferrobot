//! Tracks connected runners and wakes their dispatch loops.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use ferrobot_core::AgentId;
use ferrobot_core::protocol::DaemonFrame;
use tokio::sync::{Notify, mpsc};

#[derive(Debug)]
struct Conn {
    id: u64,
    out: mpsc::Sender<DaemonFrame>,
    current_turn: Option<String>,
    /// The runner's `protocol_version`.
    protocol: u32,
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
    /// Last time something happened for the agent (connect, message, turn).
    activity: HashMap<AgentId, Instant>,
    /// When the sandbox was last (re)started by the daemon.
    started: HashMap<AgentId, Instant>,
    /// Serializes sandbox lifecycle operations per agent.
    lifecycle: HashMap<AgentId, Arc<tokio::sync::Mutex<()>>>,
    /// Agents whose runner was refused since it last connected.
    refused: HashSet<AgentId>,
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
    pub fn register(&self, agent: AgentId, out: mpsc::Sender<DaemonFrame>, protocol: u32) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut inner = self.lock();
        inner.activity.insert(agent, Instant::now());
        inner.conns.insert(
            agent,
            Conn {
                id,
                out,
                current_turn: None,
                protocol,
            },
        );
        id
    }

    /// The connected runner's protocol version.
    pub fn runner_protocol(&self, agent: AgentId) -> Option<u32> {
        self.lock().conns.get(&agent).map(|c| c.protocol)
    }

    /// Records whether the agent's runner was refused; true if that changed.
    pub fn set_refused(&self, agent: AgentId, refused: bool) -> bool {
        let mut inner = self.lock();
        if refused {
            inner.refused.insert(agent)
        } else {
            inner.refused.remove(&agent)
        }
    }

    pub fn is_refused(&self, agent: AgentId) -> bool {
        self.lock().refused.contains(&agent)
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

    /// True while the agent's runner is executing a turn.
    pub fn is_working(&self, agent: AgentId) -> bool {
        self.lock()
            .conns
            .get(&agent)
            .is_some_and(|c| c.current_turn.is_some())
    }

    /// Lock that serializes wake/stop/restore/delete for one agent.
    pub fn lifecycle_lock(&self, agent: AgentId) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(self.lock().lifecycle.entry(agent).or_default())
    }

    /// Records activity (resets the idle timer).
    pub fn touch(&self, agent: AgentId) {
        self.lock().activity.insert(agent, Instant::now());
    }

    /// Last recorded activity; starts the clock now if there is none yet.
    pub fn last_activity(&self, agent: AgentId) -> Instant {
        *self
            .lock()
            .activity
            .entry(agent)
            .or_insert_with(Instant::now)
    }

    /// Records that the daemon (re)started the agent's sandbox.
    pub fn mark_started(&self, agent: AgentId) {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.started.insert(agent, now);
        inner.activity.insert(agent, now);
    }

    /// When the daemon last started the sandbox, if it did.
    pub fn started_at(&self, agent: AgentId) -> Option<Instant> {
        self.lock().started.get(&agent).copied()
    }

    /// Sends a frame to the agent's runner if connected. Returns whether it was queued.
    pub fn send(&self, agent: AgentId, frame: DaemonFrame) -> bool {
        self.lock()
            .conns
            .get(&agent)
            .is_some_and(|c| c.out.try_send(frame).is_ok())
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
        inner.activity.remove(&agent);
        inner.started.remove(&agent);
        inner.refused.remove(&agent);
    }

    /// Wakes the agent's dispatch loop (no-op if nothing is waiting yet; the
    /// stored permit makes the next wait return immediately).
    pub fn wake(&self, agent: AgentId) {
        self.notify_handle(agent).notify_one();
    }
}
