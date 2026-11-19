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
    // v2: agent profiles, unread tracking, cron routines with run history, snapshots
    r"
    ALTER TABLE agents ADD COLUMN description TEXT NOT NULL DEFAULT '';
    ALTER TABLE agents ADD COLUMN avatar BLOB;
    ALTER TABLE agents ADD COLUMN avatar_mime TEXT;
    ALTER TABLE agents ADD COLUMN avatar_rev INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE agents ADD COLUMN last_read_event_id INTEGER NOT NULL DEFAULT 0;

    -- The v1 routines table was never used by a scheduler.
    DROP TABLE routines;
    CREATE TABLE routines (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
        name        TEXT NOT NULL,
        schedule    TEXT NOT NULL,
        timezone    TEXT NOT NULL,
        prompt      TEXT NOT NULL,
        enabled     INTEGER NOT NULL DEFAULT 1,
        next_run_at TEXT,
        last_run_at TEXT,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL,
        UNIQUE (agent_id, name)
    );
    CREATE INDEX routines_due ON routines(enabled, next_run_at);

    CREATE TABLE routine_runs (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        routine_id INTEGER NOT NULL REFERENCES routines(id) ON DELETE CASCADE,
        message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL,
        trigger    TEXT NOT NULL CHECK (trigger IN ('schedule','manual')),
        created_at TEXT NOT NULL
    );
    CREATE INDEX routine_runs_routine ON routine_runs(routine_id, id);

    CREATE TABLE snapshots (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        agent_id     TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
        backend      TEXT NOT NULL,
        snapshot_ref TEXT NOT NULL,
        label        TEXT NOT NULL,
        scope        TEXT NOT NULL,
        created_at   TEXT NOT NULL
    );
    CREATE INDEX snapshots_agent ON snapshots(agent_id, id);
    ",
    // v3: renamable agents. Sandbox resources keep the name they were created with.
    r"
    ALTER TABLE agents ADD COLUMN sandbox_name TEXT;
    UPDATE agents SET sandbox_name = name;
    CREATE UNIQUE INDEX agents_sandbox_name ON agents(sandbox_name);
    ",
    // v4: message priority (1 urgent, 0 normal, -1 deferred: does not wake)
    r"
    ALTER TABLE messages ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;
    ",
    // v5: chosen character avatar look (JSON), NULL = derived from the id
    r"
    ALTER TABLE agents ADD COLUMN character TEXT;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_database_upgrades_in_place() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO agents (id, name, instructions, status, created_at)
             VALUES ('00000000-0000-4000-8000-000000000001', 'alice', 'x', 'running',
                     '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        run(&mut conn).unwrap();
        let (desc, rev): (String, i64) = conn
            .query_row("SELECT description, avatar_rev FROM agents", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((desc.as_str(), rev), ("", 0));
        let sandbox: String = conn
            .query_row("SELECT sandbox_name FROM agents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sandbox, "alice");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, i64::try_from(MIGRATIONS.len()).unwrap());
    }
}
