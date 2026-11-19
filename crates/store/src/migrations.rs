//! Forward-only schema migrations tracked with `PRAGMA user_version`.

use rusqlite::Connection;

use crate::StoreError;

const MIGRATIONS: &[&str] = &[
    // v1: initial schema
    r"
    CREATE TABLE agents (
        id           TEXT PRIMARY KEY,
        name         TEXT NOT NULL UNIQUE,
        instructions TEXT NOT NULL,
        status       TEXT NOT NULL,
        backend      TEXT,
        sandbox_id   TEXT,
        session_id   TEXT,
        token_hash   TEXT,
        created_at   TEXT NOT NULL
    );
    CREATE UNIQUE INDEX agents_token_hash ON agents(token_hash) WHERE token_hash IS NOT NULL;

    CREATE TABLE messages (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        sender       TEXT NOT NULL,
        recipient    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
        body         TEXT NOT NULL,
        reply_to     INTEGER REFERENCES messages(id) ON DELETE SET NULL,
        status       TEXT NOT NULL CHECK (status IN ('pending','in_flight','delivered','failed')),
        attempts     INTEGER NOT NULL DEFAULT 0,
        last_error   TEXT,
        created_at   TEXT NOT NULL,
        updated_at   TEXT NOT NULL
    );
    CREATE INDEX messages_queue ON messages(recipient, status, id);

    CREATE TABLE memories (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        agent_id   TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
        key        TEXT NOT NULL,
        content    TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        UNIQUE (agent_id, key)
    );
    CREATE VIRTUAL TABLE memories_fts USING fts5(
        key, content, content='memories', content_rowid='id', tokenize='unicode61'
    );
    CREATE TRIGGER memories_ai AFTER INSERT ON memories BEGIN
        INSERT INTO memories_fts(rowid, key, content) VALUES (new.id, new.key, new.content);
    END;
    CREATE TRIGGER memories_ad AFTER DELETE ON memories BEGIN
        INSERT INTO memories_fts(memories_fts, rowid, key, content)
        VALUES ('delete', old.id, old.key, old.content);
    END;
    CREATE TRIGGER memories_au AFTER UPDATE ON memories BEGIN
        INSERT INTO memories_fts(memories_fts, rowid, key, content)
        VALUES ('delete', old.id, old.key, old.content);
        INSERT INTO memories_fts(rowid, key, content) VALUES (new.id, new.key, new.content);
    END;

    CREATE TABLE events (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        agent_id   TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
        turn_id    TEXT,
        kind       TEXT NOT NULL,
        payload    TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    CREATE INDEX events_agent ON events(agent_id, id);

    -- Routines: schema only in phase 0; the scheduler lands in phase 1.
    CREATE TABLE routines (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
        name        TEXT NOT NULL,
        interval_s  INTEGER NOT NULL CHECK (interval_s >= 60),
        prompt      TEXT NOT NULL,
        enabled     INTEGER NOT NULL DEFAULT 1,
        next_due_at TEXT NOT NULL,
        created_at  TEXT NOT NULL,
        UNIQUE (agent_id, name)
    );
    CREATE INDEX routines_due ON routines(enabled, next_due_at);
    ",
];

pub(crate) fn run(conn: &mut Connection) -> Result<(), StoreError> {
    let current: usize = conn
        .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
        .map(|v| usize::try_from(v).unwrap_or(0))?;
    if current > MIGRATIONS.len() {
        return Err(StoreError::Corrupt(format!(
            "database schema v{current} is newer than this build (v{})",
            MIGRATIONS.len()
        )));
    }
    for (idx, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        let version = i64::try_from(idx + 1).map_err(StoreError::corrupt)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}
