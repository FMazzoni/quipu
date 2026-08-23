-- PRAGMAs are applied by `db::open` before this DDL runs. They cannot live here
-- because `rusqlite::Connection::execute_batch` wraps multi-statement strings in
-- an implicit BEGIN/COMMIT, and `PRAGMA journal_mode = WAL` is silently a no-op
-- inside a transaction.

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- stamped on init: meta(key='project_uuid', value=<uuid v4>)
-- stamped on init: meta(key='schema_version', value=db::SCHEMA_VERSION)
-- stamped on init: meta(key='display_prefix', value='QP' or user-supplied)

CREATE TABLE IF NOT EXISTS default_tag (
    name TEXT PRIMARY KEY
);

CREATE TABLE IF NOT EXISTS task (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    display_id TEXT NOT NULL UNIQUE,
    title      TEXT NOT NULL,
    tier       TEXT,
    description TEXT,
    state      TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
-- state ∈ pending | ready | assigned | running | done | cancelled

CREATE TABLE IF NOT EXISTS dep (
    task_id            INTEGER NOT NULL REFERENCES task(id),
    depends_on_task_id INTEGER NOT NULL REFERENCES task(id),
    mode               TEXT NOT NULL DEFAULT 'blocks',
    PRIMARY KEY (task_id, depends_on_task_id)
);
-- mode ∈ blocks | contains. Both make `task_id` wait for `depends_on_task_id`;
-- they differ in what that waiting means, and only `contains` propagates a
-- container's blockers down to its contents. Values match the CLI verbs
-- (`qp depends`, `qp contains`) so an edge is traceable to the command that
-- wrote it. `mode` is an attribute of the pair, not part of its identity, so
-- the PRIMARY KEY is unchanged: a pair has exactly one edge, in one mode.
--
-- Existing rows default to 'blocks', which is what makes adopting this inert:
-- a store predating the column has no containment edges, nothing to propagate
-- through, and identical behaviour until someone runs `qp contains`.
--
-- Adding this column to schema.sql was NOT enough to get it onto an existing
-- store; see the explicit ALTER in `db::add_missing_columns`.

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

-- At most one OPEN assignment per task. Before QP-142 this held only inductively:
-- every command leaving `assigned`/`running` closed its assignment in the same
-- transaction, an eight-module argument with nothing enforcing it. The partial
-- unique index makes it structural, so `assign`'s `stale_open_assignment` guard
-- degrades from a live defence to a genuine assertion.
--
-- Closed rows are exempt by design: `completed_at IS NULL` keeps the index off
-- the history, so a task can accumulate any number of completed assignments.
-- A plain UNIQUE(task_id, completed_at) would NOT work — SQLite treats each NULL
-- as distinct, so it would constrain the closed rows and permit the open ones,
-- exactly backwards.
CREATE UNIQUE INDEX IF NOT EXISTS idx_assign_one_open
    ON assignment(task_id) WHERE completed_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_rel_from     ON relation(from_task_id, kind);
CREATE INDEX IF NOT EXISTS idx_rel_to       ON relation(to_task_id, kind);

-- ---------------------------------------------------------------------------
-- Import bookkeeping. Written only by `qp import`; empty in a store that never
-- imported, which is why none of it is referenced by any other command.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS task_origin (
    task_id        INTEGER PRIMARY KEY,
    origin_uuid    TEXT NOT NULL,
    origin_task_id INTEGER NOT NULL,
    UNIQUE (origin_uuid, origin_task_id)
);
-- The canonical name of a task that was created in ANOTHER store: the uuid of
-- the store that minted it plus the rowid it holds there. A task with no row
-- here is native, and its canonical name is (this store's project_uuid, its
-- own rowid) — which is why `qp add` writes nothing here and never has to.
--
-- No foreign key to task(id) on purpose: a merge rebuilds the whole projection
-- from the merged log, deleting and reinserting every task row, and this table
-- is the thing that has to survive that in order for the reinserted rows to
-- land on the same local ids. It is the mapping, not a property of the row.
--
-- It also rides in every exported batch header. A peer reading our batch sees
-- events carrying OUR local rowids; without the map it cannot tell that our
-- task 4 is its own task 1, and the same ticket merges as two.

CREATE TABLE IF NOT EXISTS peer_task_origin (
    store_uuid     TEXT NOT NULL,
    store_task_id  INTEGER NOT NULL,
    origin_uuid    TEXT NOT NULL,
    origin_task_id INTEGER NOT NULL,
    PRIMARY KEY (store_uuid, store_task_id)
);
-- What a PEER told us about its own numbering, learned from its batch headers.
--
-- Its events stay in `foreign_event` forever, referencing tasks by the rowids
-- they hold in that peer's store; the header that explained those rowids is
-- read once and then not in reach again. Merging a third store's directory
-- would otherwise re-replay the second store's events with nothing left to
-- decode them, and every ticket the peer had itself imported would split in
-- two. This is that explanation, kept.

CREATE TABLE IF NOT EXISTS foreign_event (
    origin_uuid TEXT NOT NULL,
    origin_id   INTEGER NOT NULL,
    task_id     INTEGER,
    ts          TEXT NOT NULL,
    kind        TEXT NOT NULL,
    agent_id    TEXT,
    payload     TEXT,
    PRIMARY KEY (origin_uuid, origin_id)
);
-- Events merged in from another store. Deliberately NOT in `event`: that table
-- is what `qp export` writes out, and re-exporting a peer's events under our
-- own uuid and a fresh id would mint a second identity for an event that
-- already has one, breaking the dedupe every reader depends on. Each machine
-- exports what it wrote; the union of the batches is the whole log.
--
-- `task_id` is the ORIGIN store's rowid, resolved through `task_origin` (and
-- the batch header's copy of it) at replay time. The PRIMARY KEY is what makes
-- importing the same batch twice a no-op.

CREATE TABLE IF NOT EXISTS conflict (
    task_id         INTEGER NOT NULL,
    field           TEXT NOT NULL,
    origin_uuid     TEXT NOT NULL,
    origin_event_id INTEGER NOT NULL,
    base            TEXT,
    value           TEXT,
    ts              TEXT NOT NULL,
    PRIMARY KEY (task_id, field, origin_uuid, origin_event_id)
);
-- Two edits to one field that both claim the same prior value are concurrent
-- by definition, and neither may be discarded. Both candidate values are
-- recorded here, the field itself is left at the `base` they agree on, and the
-- task is tagged for a human to settle. Rebuilt from scratch by every merge.

CREATE INDEX IF NOT EXISTS idx_foreign_event_ts ON foreign_event(ts, origin_uuid, origin_id);
