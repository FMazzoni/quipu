//! quipu storage layer: SQLite open/migrate, transaction helpers, error types,
//! and shared mutation utilities. Every state mutation in the crate routes
//! through `with_tx` + a guarded conditional UPDATE — see
//! `docs/DECISIONS.md → guarded-state-transitions.md` for the contract.

use anyhow::{Context, Result};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::path::{Path, PathBuf};
use thiserror::Error;

const SCHEMA: &str = include_str!("schema.sql");

pub const STATE_PENDING:   &str = "pending";
pub const STATE_READY:     &str = "ready";
pub const STATE_ASSIGNED:  &str = "assigned";
pub const STATE_RUNNING:   &str = "running";
pub const STATE_DONE:      &str = "done";
pub const STATE_BLOCKED:   &str = "blocked";
pub const STATE_CANCELLED: &str = "cancelled";

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

/// One-liner wrapper for converting rusqlite errors into the domain error type.
/// Used as `.map_err(map_sqlite)`.
pub fn map_sqlite(e: rusqlite::Error) -> anyhow::Error {
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

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
    let conn = Connection::open(path)
        .with_context(|| format!("opening sqlite at {}", path.display()))?;
    conn.execute_batch(SCHEMA).context("applying schema")?;
    // Stamp project_uuid on first init.
    let existing: Option<String> = conn.query_row(
        "SELECT value FROM meta WHERE key='project_uuid'", [], |r| r.get(0)).ok();
    if existing.is_none() {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('project_uuid', ?), ('schema_version', '1')",
            [uuid::Uuid::new_v4().to_string()])?;
    }
    Ok(conn)
}

pub fn with_tx<T>(conn: &mut Connection, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let v = f(&tx)?;
    tx.commit()?;
    Ok(v)
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
