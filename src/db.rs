//! quipu storage layer: SQLite open/migrate, transaction helpers, error types,
//! and shared mutation utilities. Every state mutation in the crate routes
//! through `with_tx` + a guarded conditional UPDATE — see
//! `docs/DECISIONS.md → guarded-state-transitions.md` for the contract.

use anyhow::{Context, Result};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::path::{Path, PathBuf};
use thiserror::Error;

const SCHEMA: &str = include_str!("schema.sql");

/// Typed state of a task in the workflow. Use this anywhere the database
/// schema's `state` column is read or written; the `&str` constants below
/// remain as aliases for ergonomic `WHERE state IN (...)` SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    Pending,
    Ready,
    Assigned,
    Running,
    Done,
    Blocked,
    Cancelled,
}

impl State {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Pending   => "pending",
            Self::Ready     => "ready",
            Self::Assigned  => "assigned",
            Self::Running   => "running",
            Self::Done      => "done",
            Self::Blocked   => "blocked",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending"   => Some(Self::Pending),
            "ready"     => Some(Self::Ready),
            "assigned"  => Some(Self::Assigned),
            "running"   => Some(Self::Running),
            "done"      => Some(Self::Done),
            "blocked"   => Some(Self::Blocked),
            "cancelled" => Some(Self::Cancelled),
            _           => None,
        }
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl rusqlite::ToSql for State {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

// Legacy `&str` constants — kept as thin aliases over `State::*.as_str()` so
// existing call sites (and the Task 4–7 plan blocks) keep working. New code
// should prefer the typed `State` variant.
pub const STATE_PENDING:   &str = State::Pending.as_str();
pub const STATE_READY:     &str = State::Ready.as_str();
pub const STATE_ASSIGNED:  &str = State::Assigned.as_str();
pub const STATE_RUNNING:   &str = State::Running.as_str();
pub const STATE_DONE:      &str = State::Done.as_str();
pub const STATE_BLOCKED:   &str = State::Blocked.as_str();
pub const STATE_CANCELLED: &str = State::Cancelled.as_str();

/// Typed errors. `main` matches on the variant to pick an exit code.
#[derive(Debug, Error)]
pub enum QuipuError {
    /// Constraint violation (wrong state, wrong assignee, double-assign, etc.) — exit 2.
    #[error("{0}")]
    Constraint(String),
    /// Referenced row not found — exit 1, but distinct error message.
    #[error("not found: {0}")]
    NotFound(String),
    /// Invalid CLI input — exit 1.
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

pub fn constraint(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(QuipuError::Constraint(msg.into()))
}

pub fn not_found(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(QuipuError::NotFound(msg.into()))
}

/// Converts rusqlite errors into the domain error type, preserving constraint
/// semantics so `main` can map them to exit code 2. `SQLITE_CONSTRAINT` (UNIQUE,
/// FK, CHECK, NOT NULL) becomes `QuipuError::Constraint(extended-message)`;
/// everything else passes through opaquely as `anyhow::Error::new(e)`.
/// Used as `.map_err(map_sqlite)`.
pub fn map_sqlite(e: rusqlite::Error) -> anyhow::Error {
    if let rusqlite::Error::SqliteFailure(ref ferr, _) = e {
        if ferr.code == rusqlite::ErrorCode::ConstraintViolation {
            return anyhow::Error::from(QuipuError::Constraint(e.to_string()));
        }
    }
    anyhow::Error::new(e)
}

pub fn resolve_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit { return Ok(p); }
    let cwd = std::env::current_dir()?;
    for a in cwd.ancestors() {
        let c = a.join(".quipu").join("db.sqlite");
        if c.exists() { return Ok(c); }
    }
    Ok(cwd.join(".quipu").join("db.sqlite"))
}

/// Detect when `--db`/`QP_DB` points at a different repo than the cwd's discovered store.
/// Prints a warning to stderr; never errors.
pub fn warn_on_project_mismatch(explicit: &Option<PathBuf>) -> Result<()> {
    let Some(explicit_path) = explicit else { return Ok(()); };
    let cwd = std::env::current_dir()?;
    let mut local: Option<PathBuf> = None;
    for a in cwd.ancestors() {
        let c = a.join(".quipu").join("db.sqlite");
        if c.exists() { local = Some(c); break; }
    }
    let Some(local) = local else { return Ok(()); };
    if local.canonicalize().ok() == explicit_path.canonicalize().ok() { return Ok(()); }
    let uuid_explicit = read_project_uuid(explicit_path).ok().flatten();
    let uuid_local    = read_project_uuid(&local).ok().flatten();
    if let (Some(a), Some(b)) = (uuid_explicit, uuid_local) {
        if a != b {
            eprintln!("warning: project_uuid mismatch — QP_DB={} (uuid {}) but cwd resolves to {} (uuid {})",
                explicit_path.display(), a, local.display(), b);
        }
    }
    Ok(())
}

fn read_project_uuid(path: &Path) -> Result<Option<String>> {
    if !path.exists() { return Ok(None); }
    let conn = Connection::open(path)?;
    let v: Option<String> = conn.query_row(
        "SELECT value FROM meta WHERE key = 'project_uuid'", [], |r| r.get(0)
    ).ok();
    Ok(v)
}

/// The schema version this binary was compiled against. Bump whenever
/// `schema.sql` changes in a way that requires a migration. The value is
/// stamped into `meta(key='schema_version')` on first init; subsequent
/// `open()` calls compare it to decide whether to (re)apply DDL.
pub const SCHEMA_VERSION: &str = "1";

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
    let conn = Connection::open(path)
        .with_context(|| format!("opening sqlite at {}", path.display()))?;

    // PRAGMAs must run outside any transaction. `PRAGMA journal_mode = WAL` is
    // silently a no-op inside an implicit BEGIN/COMMIT wrapper, which is what
    // `execute_batch` does for multi-statement strings.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "busy_timeout", 5000)?;

    // Migration contract: read the stamped schema_version. If it matches
    // SCHEMA_VERSION, DDL is up to date and we skip the schema re-apply on the
    // hot path (saves 1–3 ms per invocation on already-initialized stores).
    // If it is missing, this is either a fresh db or a pre-versioning store —
    // apply the DDL and stamp meta rows via INSERT OR IGNORE (idempotent, no
    // read-then-write).
    let current: Option<String> = conn.query_row(
        "SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0)
    ).ok();
    if current.as_deref() != Some(SCHEMA_VERSION) {
        conn.execute_batch(SCHEMA).context("applying schema")?;
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('project_uuid', ?1), ('schema_version', ?2)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), SCHEMA_VERSION])?;
    }
    Ok(conn)
}

pub fn with_tx<T>(conn: &mut Connection, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    match f(&tx) {
        Ok(v) => { tx.commit()?; Ok(v) }
        Err(e) => { let _ = tx.rollback(); Err(e) }
    }
}

pub fn host() -> String {
    std::env::var("HOST").or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

pub fn insert_event(
    tx: &Transaction,
    task_id: Option<i64>,
    kind: &str,
    agent_id: Option<&str>,
    payload: Option<&serde_json::Value>,
) -> Result<i64> {
    let s = payload.map(|p| p.to_string());
    tx.execute(
        "INSERT INTO event(task_id, kind, agent_id, payload) VALUES (?,?,?,?)",
        rusqlite::params![task_id, kind, agent_id, s])?;
    Ok(tx.last_insert_rowid())
}

/// Re-derive readiness: any pending task whose deps are all done/cancelled becomes ready.
pub fn refresh_ready(tx: &Transaction) -> Result<()> {
    tx.execute(
        "UPDATE task
            SET state = 'ready'
          WHERE state = 'pending'
            AND NOT EXISTS (
              SELECT 1 FROM dep d
              JOIN task t2 ON t2.id = d.depends_on_task_id
              WHERE d.task_id = task.id
                AND t2.state NOT IN ('done','cancelled')
            )",
        [])?;
    Ok(())
}

/// Recursive check: would adding `from -depends_on-> to` create a cycle?
pub fn would_cycle(tx: &Transaction, from: i64, to: i64) -> Result<bool> {
    if from == to { return Ok(true); }
    // From `to`, can we reach `from` via depends_on edges? If yes → cycle.
    let n: i64 = tx.query_row(
        "WITH RECURSIVE reach(id) AS (
           SELECT depends_on_task_id FROM dep WHERE task_id = ?1
           UNION
           SELECT d.depends_on_task_id FROM dep d JOIN reach r ON r.id = d.task_id
         )
         SELECT COUNT(*) FROM reach WHERE id = ?2",
        [to, from], |r| r.get(0))?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_str_round_trip() {
        for s in [
            State::Pending, State::Ready, State::Assigned, State::Running,
            State::Done, State::Blocked, State::Cancelled,
        ] {
            assert_eq!(State::from_str(s.as_str()), Some(s));
            assert_eq!(s.to_string(), s.as_str());
        }
        assert_eq!(State::from_str("nope"), None);
    }

    #[test]
    fn state_constants_alias_enum() {
        assert_eq!(STATE_PENDING,   State::Pending.as_str());
        assert_eq!(STATE_READY,     State::Ready.as_str());
        assert_eq!(STATE_ASSIGNED,  State::Assigned.as_str());
        assert_eq!(STATE_RUNNING,   State::Running.as_str());
        assert_eq!(STATE_DONE,      State::Done.as_str());
        assert_eq!(STATE_BLOCKED,   State::Blocked.as_str());
        assert_eq!(STATE_CANCELLED, State::Cancelled.as_str());
    }
}
