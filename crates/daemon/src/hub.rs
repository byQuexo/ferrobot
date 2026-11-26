//! Tracks connected runners and wakes their dispatch loops.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use ferrobot_core::AgentId;
use ferrobot_core::protocol::DaemonFrame;
use tokio::sync::{Notify, mpsc};

use crate::views::{CredentialError, ToolsUnsupported};

#[derive(Debug)]
struct Conn {
    id: u64,
    out: mpsc::Sender<DaemonFrame>,
    current_turn: Option<String>,
    /// The runner's `protocol_version`.
    protocol: u32,
    /// Whether the runner takes messages into a running turn.
    steering: bool,
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
    /// Agents whose model credential is missing or rejected; their turns wait.
    credentials: HashMap<AgentId, CredentialError>,
    /// Agents whose model refused tools; their turns wait too.
    no_tools: HashMap<AgentId, ToolsUnsupported>,
}

impl Hub {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        crate::lock(&self.inner)
    }

    /// The wake-up signal for an agent's dispatch loop.
    pub fn notify_handle(&self, agent: AgentId) -> Arc<Notify> {
        Arc::clone(self.lock().notifies.entry(agent).or_default())
    }

    /// Registers a connection, replacing (and thereby closing) an older one.
    /// Returns the connection id used for unregistering.
    pub fn register(
        &self,
        agent: AgentId,
        out: mpsc::Sender<DaemonFrame>,
        protocol: u32,
        steering: bool,
    ) -> u64 {
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
                steering,
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

    /// Records the agent's credential problem unless one is already known; true if it is new.
    pub fn set_credential_error(&self, agent: AgentId, error: CredentialError) -> bool {
        match self.lock().credentials.entry(agent) {
            Entry::Occupied(_) => false,
            Entry::Vacant(e) => {
                e.insert(error);
                true
            }
        }
    }

    /// Forgets the agent's credential problem; true if there was one.
    pub fn clear_credential_error(&self, agent: AgentId) -> bool {
        self.lock().credentials.remove(&agent).is_some()
    }

    pub fn credential_error(&self, agent: AgentId) -> Option<CredentialError> {
        self.lock().credentials.get(&agent).cloned()
    }

    /// Records that the agent's model refused tools unless already known; true if it is new.
    pub fn set_tools_unsupported(&self, agent: AgentId, problem: ToolsUnsupported) -> bool {
        match self.lock().no_tools.entry(agent) {
            Entry::Occupied(_) => false,
            Entry::Vacant(e) => {
                e.insert(problem);
                true
            }
        }
    }

    /// Forgets that the agent's model refused tools; true if it had.
    pub fn clear_tools_unsupported(&self, agent: AgentId) -> bool {
        self.lock().no_tools.remove(&agent).is_some()
    }

    pub fn tools_unsupported(&self, agent: AgentId) -> Option<ToolsUnsupported> {
        self.lock().no_tools.get(&agent).cloned()
    }

    /// True while a model problem holds the agent's turns.
    pub fn held(&self, agent: AgentId) -> bool {
        let inner = self.lock();
        inner.credentials.contains_key(&agent) || inner.no_tools.contains_key(&agent)
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

    /// True while the agent is in a turn its runner cannot hand a new message to: the message
    /// waits for the turn to end.
    // shortcut: a steering runner's call room turn does not take chat messages either; it counts as taking them here.
    pub fn turn_holds_messages(&self, agent: AgentId) -> bool {
        self.lock()
            .conns
            .get(&agent)
            .is_some_and(|c| c.current_turn.is_some() && !c.steering)
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

    /// The turn `agent`'s runner is working on, if any.
    pub fn current_turn(&self, agent: AgentId) -> Option<String> {
        self.lock().conns.get(&agent)?.current_turn.clone()
    }

    /// Asks the runner to abort `turn`, if that is the turn it is working on.
    pub fn interrupt_turn(&self, agent: AgentId, turn: &str) {
        let inner = self.lock();
        if let Some(conn) = inner
            .conns
            .get(&agent)
            .filter(|c| c.current_turn.as_deref() == Some(turn))
        {
            let _ = conn.out.try_send(DaemonFrame::Interrupt {
                turn_id: turn.to_owned(),
            });
        }
    }

    pub fn forget(&self, agent: AgentId) {
        let mut inner = self.lock();
        inner.conns.remove(&agent);
        inner.notifies.remove(&agent);
        inner.activity.remove(&agent);
        inner.started.remove(&agent);
        inner.refused.remove(&agent);
        inner.credentials.remove(&agent);
        inner.no_tools.remove(&agent);
    }

    /// Wakes the agent's dispatch loop (no-op if nothing is waiting yet; the
    /// stored permit makes the next wait return immediately).
    pub fn wake(&self, agent: AgentId) {
        self.notify_handle(agent).notify_one();
    }
}
