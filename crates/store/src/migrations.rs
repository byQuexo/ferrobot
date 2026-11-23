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
    // v6: who created the agent: 'operator' or the creating agent's id
    r"
    ALTER TABLE agents ADD COLUMN created_by TEXT NOT NULL DEFAULT 'operator';
    ",
    // v7: model providers; agents pick one (NULL = the daemon's ANTHROPIC_API_KEY) and a model
    r"
    CREATE TABLE providers (
        id            TEXT PRIMARY KEY,
        name          TEXT NOT NULL UNIQUE,
        kind          TEXT NOT NULL,
        base_url      TEXT,
        region        TEXT,
        project       TEXT,
        default_model TEXT,
        created_at    TEXT NOT NULL
    );
    ALTER TABLE agents ADD COLUMN provider_id TEXT REFERENCES providers(id);
    ALTER TABLE agents ADD COLUMN model TEXT;
    ",
    // v8: global memory, shared by all agents
    r"
    CREATE TABLE global_memories (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        key        TEXT NOT NULL UNIQUE,
        content    TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE VIRTUAL TABLE global_memories_fts USING fts5(
        key, content, content='global_memories', content_rowid='id', tokenize='unicode61'
    );
    CREATE TRIGGER global_memories_ai AFTER INSERT ON global_memories BEGIN
        INSERT INTO global_memories_fts(rowid, key, content) VALUES (new.id, new.key, new.content);
    END;
    CREATE TRIGGER global_memories_ad AFTER DELETE ON global_memories BEGIN
        INSERT INTO global_memories_fts(global_memories_fts, rowid, key, content)
        VALUES ('delete', old.id, old.key, old.content);
    END;
    CREATE TRIGGER global_memories_au AFTER UPDATE ON global_memories BEGIN
        INSERT INTO global_memories_fts(global_memories_fts, rowid, key, content)
        VALUES ('delete', old.id, old.key, old.content);
        INSERT INTO global_memories_fts(rowid, key, content) VALUES (new.id, new.key, new.content);
    END;
    ",
    // v9: connectors (remote MCP servers) and which agents may use them
    r"
    CREATE TABLE connectors (
        id         TEXT PRIMARY KEY,
        name       TEXT NOT NULL UNIQUE,
        url        TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    CREATE TABLE agent_connectors (
        agent_id     TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
        connector_id TEXT NOT NULL REFERENCES connectors(id) ON DELETE CASCADE,
        PRIMARY KEY (agent_id, connector_id)
    );
    ",
    // v10: who wrote a global memory (an agent id); NULL = the operator
    r"
    ALTER TABLE global_memories ADD COLUMN author TEXT;
    ",
    // v11: agents pinned to the top of the operator's list
    r"
    ALTER TABLE agents ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
    ",
    // v12: daemon-wide settings as JSON values by key (e.g. 'voice'); no secrets
    r"
    CREATE TABLE settings (
        key        TEXT PRIMARY KEY,
        value      TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    ",
    // v13: model usage per proxied call; no agent FK so rows outlive deleted agents
    r"
    CREATE TABLE usage (
        id                 INTEGER PRIMARY KEY AUTOINCREMENT,
        at                 TEXT NOT NULL,
        agent_id           TEXT NOT NULL,
        agent_name         TEXT NOT NULL,
        provider_id        TEXT,
        provider_kind      TEXT NOT NULL,
        model              TEXT NOT NULL,
        input_tokens       INTEGER,
        output_tokens      INTEGER,
        cache_read_tokens  INTEGER,
        cache_write_tokens INTEGER,
        latency_ms         INTEGER NOT NULL,
        status             TEXT NOT NULL CHECK (status IN ('ok','refused','error'))
    );
    CREATE INDEX usage_at ON usage(at);
    ",
    // v14: the USD a call cost as the upstream reported it (OpenRouter's `usage.cost`)
    "ALTER TABLE usage ADD COLUMN cost_usd REAL;",
    // v15: the tool groups an agent may use, a JSON array; NULL = every group
    "ALTER TABLE agents ADD COLUMN tools TEXT;",
    // v16: call rooms (calls with two or more agents), their members and lines; messages that
    // carry a room turn name their room. No agent FKs, so a room outlives deleted agents.
    r"
    CREATE TABLE rooms (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        created_at TEXT NOT NULL,
        ended_at   TEXT
    );
    CREATE TABLE room_members (
        room_id   INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
        agent_id  TEXT NOT NULL,
        joined_at TEXT NOT NULL,
        PRIMARY KEY (room_id, agent_id)
    );
    CREATE TABLE room_lines (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        room_id    INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
        speaker    TEXT NOT NULL,
        body       TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    CREATE INDEX room_lines_room ON room_lines(room_id, id);
    ALTER TABLE messages ADD COLUMN room_id INTEGER REFERENCES rooms(id);
    CREATE INDEX events_turn ON events(turn_id) WHERE turn_id IS NOT NULL;
    ",
    // v17: the agent a room line invited (the line is the topic it got); NULL = words said
    "ALTER TABLE room_lines ADD COLUMN invited TEXT;",
    // v18: the operator's tags on an agent, a JSON array; NULL = none. An agent's newest usage
    // row is its context size.
    r"
    ALTER TABLE agents ADD COLUMN tags TEXT;
    CREATE INDEX usage_agent ON usage(agent_id, id);
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
        let created_by: String = conn
            .query_row("SELECT created_by FROM agents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            created_by, "operator",
            "existing agents count as operator-created"
        );
        let (provider, model): (Option<String>, Option<String>) = conn
            .query_row("SELECT provider_id, model FROM agents", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(
            (provider, model),
            (None, None),
            "existing agents keep the daemon's default model"
        );
        let providers: i64 = conn
            .query_row("SELECT COUNT(*) FROM providers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(providers, 0);
        let global: i64 = conn
            .query_row("SELECT COUNT(*) FROM global_memories", [], |r| r.get(0))
            .unwrap();
        assert_eq!(global, 0, "global memory starts empty");
        let assigned: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_connectors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(assigned, 0, "existing agents get no connectors");
        conn.execute(
            "INSERT INTO global_memories (key, content, created_at, updated_at)
             VALUES ('k', 'v', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let author: Option<String> = conn
            .query_row("SELECT author FROM global_memories", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            author, None,
            "rows without an author count as the operator's"
        );
        let pinned: bool = conn
            .query_row("SELECT pinned FROM agents", [], |r| r.get(0))
            .unwrap();
        assert!(!pinned, "existing agents are not pinned");
        let settings: i64 = conn
            .query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(settings, 0, "settings start empty: voice is off");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, i64::try_from(MIGRATIONS.len()).unwrap());
    }

    #[test]
    fn v12_database_gains_an_empty_usage_table() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..12] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 12).unwrap();
        conn.execute_batch(
            "INSERT INTO agents (id, name, instructions, status, created_at)
             VALUES ('00000000-0000-4000-8000-000000000001', 'alice', 'x', 'running',
                     '2026-01-01T00:00:00Z');
             INSERT INTO settings (key, value, updated_at)
             VALUES ('voice', '{}', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
        run(&mut conn).unwrap();
        let (name, voice): (String, String) = conn
            .query_row(
                "SELECT (SELECT name FROM agents), (SELECT value FROM settings)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((name.as_str(), voice.as_str()), ("alice", "{}"));
        let usage: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(usage, 0);
    }

    #[test]
    fn v13_usage_rows_keep_their_estimate() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..13] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 13).unwrap();
        conn.execute_batch(
            "INSERT INTO usage (at, agent_id, agent_name, provider_kind, model, input_tokens,
                                output_tokens, latency_ms, status)
             VALUES ('2026-01-01T00:00:00Z', '00000000-0000-4000-8000-000000000001', 'alice',
                     'openrouter', 'm', 10, 5, 100, 'ok');",
        )
        .unwrap();
        run(&mut conn).unwrap();
        let (tokens, cost): (i64, Option<f64>) = conn
            .query_row("SELECT input_tokens, cost_usd FROM usage", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((tokens, cost), (10, None), "old rows have no reported cost");
    }

    #[test]
    fn v14_agents_keep_every_tool() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..14] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 14).unwrap();
        conn.execute_batch(
            "INSERT INTO agents (id, name, instructions, status, created_at)
             VALUES ('00000000-0000-4000-8000-000000000001', 'alice', 'x', 'running',
                     '2026-01-01T00:00:00Z');",
        )
        .unwrap();
        run(&mut conn).unwrap();
        let tools: Option<String> = conn
            .query_row("SELECT tools FROM agents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tools, None, "NULL: existing agents keep every tool group");
    }

    #[test]
    fn v15_messages_stay_in_the_chat_and_rooms_start_empty() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..15] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 15).unwrap();
        conn.execute_batch(
            "INSERT INTO agents (id, name, instructions, status, created_at)
             VALUES ('00000000-0000-4000-8000-000000000001', 'alice', 'x', 'running',
                     '2026-01-01T00:00:00Z');
             INSERT INTO messages (sender, recipient, body, status, created_at, updated_at)
             VALUES ('user', '00000000-0000-4000-8000-000000000001', 'hi', 'delivered',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
        run(&mut conn).unwrap();
        let (body, room): (String, Option<i64>) = conn
            .query_row("SELECT body, room_id FROM messages", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((body.as_str(), room), ("hi", None), "old messages are chat");
        let rooms: i64 = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM rooms) + (SELECT COUNT(*) FROM room_lines)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rooms, 0);
    }

    #[test]
    fn v16_room_lines_stay_words_said() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..16] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 16).unwrap();
        conn.execute_batch(
            "INSERT INTO rooms (created_at) VALUES ('2026-01-01T00:00:00Z');
             INSERT INTO room_lines (room_id, speaker, body, created_at)
             VALUES (1, 'user', 'hi', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
        run(&mut conn).unwrap();
        let invited: Option<String> = conn
            .query_row("SELECT invited FROM room_lines", [], |r| r.get(0))
            .unwrap();
        assert_eq!(invited, None);
    }

    #[test]
    fn v17_agents_have_no_tags() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..17] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 17).unwrap();
        conn.execute_batch(
            "INSERT INTO agents (id, name, instructions, status, created_at)
             VALUES ('00000000-0000-4000-8000-000000000001', 'alice', 'x', 'running',
                     '2026-01-01T00:00:00Z');",
        )
        .unwrap();
        run(&mut conn).unwrap();
        let tags: Option<String> = conn
            .query_row("SELECT tags FROM agents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tags, None);
    }
}
