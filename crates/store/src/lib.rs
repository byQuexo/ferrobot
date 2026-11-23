//! The daemon's SQLite database. [`Store`] runs every query on Tokio's blocking pool, so async
//! callers never block the reactor.

mod agents;
mod connectors;
mod error;
mod events;
mod mailbox;
mod memory;
mod migrations;
mod providers;
mod rooms;
mod routines;
mod settings;
mod snapshots;
mod tasks;
mod usage;
mod util;

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

pub use agents::{AgentPatch, Avatar, NewAgent, hash_token};
pub use error::StoreError;
pub use events::StoredEvent;
pub use mailbox::AgentPair;
pub use memory::MemoryEntry;
pub use rooms::{Room, RoomLine};
pub use routines::{NewRoutine, Routine, RoutineRun, RunTrigger};
pub use snapshots::SnapshotRecord;
pub use tasks::Task;
pub use usage::{
    ContextUsage, Tokens, UsageFilter, UsageGroupBy, UsageRecord, UsageSlice, UsageStatus,
};

/// Maximum delivery attempts before a message is marked failed.
pub const MAX_DELIVERY_ATTEMPTS: u32 = 3;

/// Handle to the ferrobot database. Clone freely.
#[derive(Clone, Debug)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// Opens (and migrates) the database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Opens a private in-memory database (tests).
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        // WAL is a no-op for in-memory databases; ignore the returned mode.
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        migrations::run(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Runs `f` with the connection on the blocking thread pool.
    async fn with<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().map_err(|_| StoreError::Poisoned)?;
            f(&mut guard)
        })
        .await?
    }
}
