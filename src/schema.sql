-- PRAGMAs are applied by `db::open` before this DDL runs. They cannot live here
-- because `rusqlite::Connection::execute_batch` wraps multi-statement strings in
-- an implicit BEGIN/COMMIT, and `PRAGMA journal_mode = WAL` is silently a no-op
-- inside a transaction.

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- stamped on init: meta(key='project_uuid', value=<uuid v4>)
-- stamped on init: meta(key='schema_version', value='1')
-- stamped on init: meta(key='display_prefix', value='QP' or user-supplied)

CREATE TABLE IF NOT EXISTS task (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    display_id TEXT NOT NULL UNIQUE,
    title      TEXT NOT NULL,
    tier       TEXT,
    state      TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
-- state ∈ pending | ready | assigned | running | done | cancelled

CREATE TABLE IF NOT EXISTS dep (
    task_id            INTEGER NOT NULL REFERENCES task(id),
    depends_on_task_id INTEGER NOT NULL REFERENCES task(id),
    PRIMARY KEY (task_id, depends_on_task_id)
);

CREATE TABLE IF NOT EXISTS assignment (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id      INTEGER NOT NULL REFERENCES task(id),
    agent_id     TEXT NOT NULL,
    assigned_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    claimed_at   TEXT,
    completed_at TEXT,
    outcome      TEXT  -- success | failed | abandoned | reclaimed | cancelled
);

CREATE TABLE IF NOT EXISTS event (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id  INTEGER REFERENCES task(id),
    ts       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    kind     TEXT NOT NULL,
    agent_id TEXT,
    payload  TEXT
);
-- kind is free-form. Common: state_change, decision, note, artifact, reclaim, cancel.
-- Events are only ever inserted inside the IMMEDIATE tx of a mutation.
-- This is the watch-correctness invariant: writers serialize, so event.id is gap-free.

CREATE TABLE IF NOT EXISTS tag (
    task_id INTEGER NOT NULL REFERENCES task(id),
    name    TEXT NOT NULL,
    PRIMARY KEY (task_id, name)
);

CREATE TABLE IF NOT EXISTS relation (
    from_task_id INTEGER NOT NULL REFERENCES task(id),
    to_task_id   INTEGER NOT NULL REFERENCES task(id),
    kind         TEXT NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (from_task_id, to_task_id, kind)
);
-- kind is free-form. Common: variant-of, supersedes, fixes.
-- Directed; symmetry expressed by adding the inverse edge if desired.

CREATE INDEX IF NOT EXISTS idx_event_task   ON event(task_id, id);
CREATE INDEX IF NOT EXISTS idx_event_ts     ON event(ts);
CREATE INDEX IF NOT EXISTS idx_event_kind   ON event(kind);
CREATE INDEX IF NOT EXISTS idx_task_state   ON task(state);
CREATE INDEX IF NOT EXISTS idx_tag_name     ON tag(name, task_id);
CREATE INDEX IF NOT EXISTS idx_dep_back     ON dep(depends_on_task_id);
CREATE INDEX IF NOT EXISTS idx_assign_task  ON assignment(task_id, completed_at);
CREATE INDEX IF NOT EXISTS idx_rel_from     ON relation(from_task_id, kind);
CREATE INDEX IF NOT EXISTS idx_rel_to       ON relation(to_task_id, kind);
