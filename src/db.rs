//! quipu storage layer: SQLite open/migrate, transaction helpers, error types,
//! and shared mutation utilities. Every state mutation in the crate routes
//! through `with_tx` + a guarded conditional UPDATE — see
//! `vault decisions/ → guarded-state-transitions.md` for the contract.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
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
    Cancelled,
}

impl State {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Assigned => "assigned",
            Self::Running => "running",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    #[allow(dead_code)] // paired with as_str; used in tests
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "ready" => Some(Self::Ready),
            "assigned" => Some(Self::Assigned),
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
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
// existing call sites keep working. New code should prefer the typed `State` variant.
pub const STATE_PENDING: &str = State::Pending.as_str();
pub const STATE_READY: &str = State::Ready.as_str();
#[allow(dead_code)] // kept for family consistency; STATE_PENDING/READY used in add.rs
pub const STATE_ASSIGNED: &str = State::Assigned.as_str();
#[allow(dead_code)] // kept for family consistency
pub const STATE_RUNNING: &str = State::Running.as_str();
#[allow(dead_code)] // kept for family consistency
pub const STATE_DONE: &str = State::Done.as_str();
#[allow(dead_code)] // kept for family consistency
pub const STATE_CANCELLED: &str = State::Cancelled.as_str();

/// Typed errors. `main` matches on the variant to pick an exit code.
#[derive(Debug, Error)]
pub enum QuipuError {
    /// Constraint violation (wrong state, wrong assignee, double-assign, etc.) — exit 2.
    #[error("{0}")]
    Constraint(String),
    /// Invalid CLI input — exit 1.
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

pub fn constraint(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(QuipuError::Constraint(msg.into()))
}

pub fn invalid_input(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(QuipuError::InvalidInput(msg.into()))
}

pub fn resolve_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    let cwd = std::env::current_dir()?;
    for a in cwd.ancestors() {
        let c = a.join(".quipu").join("db.sqlite");
        if c.exists() {
            return Ok(c);
        }
    }
    // Git-aware fallback: when invoked from a worktree, the main repo's
    // `.quipu/` is a sibling of the worktree, not an ancestor. Ask git for
    // the common .git dir (resolves to the main repo's .git regardless of
    // whether we're in a worktree or the main checkout) and look for
    // `.quipu/db.sqlite` next to it.
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .output()
    {
        if out.status.success() {
            let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !raw.is_empty() {
                let git_dir = PathBuf::from(&raw);
                let git_dir = if git_dir.is_absolute() {
                    git_dir
                } else {
                    cwd.join(git_dir)
                };
                let git_dir = git_dir.canonicalize().unwrap_or(git_dir);
                if let Some(repo_root) = git_dir.parent() {
                    let c = repo_root.join(".quipu").join("db.sqlite");
                    if c.exists() {
                        return Ok(c);
                    }
                }
            }
        }
    }
    Ok(cwd.join(".quipu").join("db.sqlite"))
}

/// Detect when `--db`/`QP_DB` points at a different repo than the cwd's discovered store.
/// Prints a warning to stderr; never errors.
pub fn warn_on_project_mismatch(explicit: &Option<PathBuf>) -> Result<()> {
    let Some(explicit_path) = explicit else {
        return Ok(());
    };
    let cwd = std::env::current_dir()?;
    let mut local: Option<PathBuf> = None;
    for a in cwd.ancestors() {
        let c = a.join(".quipu").join("db.sqlite");
        if c.exists() {
            local = Some(c);
            break;
        }
    }
    let Some(local) = local else {
        return Ok(());
    };
    if local.canonicalize().ok() == explicit_path.canonicalize().ok() {
        return Ok(());
    }
    let uuid_explicit = read_project_uuid(explicit_path).ok().flatten();
    let uuid_local = read_project_uuid(&local).ok().flatten();
    if let (Some(a), Some(b)) = (uuid_explicit, uuid_local) {
        if a != b {
            eprintln!("warning: project_uuid mismatch — QP_DB={} (uuid {}) but cwd resolves to {} (uuid {})",
                explicit_path.display(), a, local.display(), b);
        }
    }
    Ok(())
}

fn read_project_uuid(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(path)?;
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'project_uuid'",
            [],
            |r| r.get(0),
        )
        .ok();
    Ok(v)
}

/// The schema version this binary was compiled against. Bump whenever
/// `schema.sql` changes in a way that requires a migration. The value is
/// stamped into `meta(key='schema_version')` on first init; subsequent
/// `open()` calls compare it to decide whether to (re)apply DDL.
pub const SCHEMA_VERSION: &str = "2";

pub fn open(path: &Path) -> Result<Connection> {
    open_with(path, None, &[])
}

#[allow(dead_code)] // retained as a stable shim; tests + external callers may still use it
pub fn open_with_prefix(path: &Path, prefix: Option<&str>) -> Result<Connection> {
    open_with(path, prefix, &[])
}

pub fn open_with(path: &Path, prefix: Option<&str>, default_tags: &[String]) -> Result<Connection> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let conn =
        Connection::open(path).with_context(|| format!("opening sqlite at {}", path.display()))?;

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
    let current: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .ok();
    if let (Some(cur), Some(user_p)) = (current.as_deref(), prefix) {
        if cur == SCHEMA_VERSION {
            let stored: Option<String> = conn
                .query_row(
                    "SELECT value FROM meta WHERE key='display_prefix'",
                    [],
                    |r| r.get(0),
                )
                .ok();
            if let Some(s) = stored.as_deref() {
                if s != user_p {
                    eprintln!(
                        "warn: prefix already set to {}; --prefix {} ignored",
                        s, user_p
                    );
                }
            }
        }
    }
    if current.as_deref() != Some(SCHEMA_VERSION) {
        conn.execute_batch(SCHEMA).context("applying schema")?;
        // Stamp project_uuid + schema_version + display_prefix on first init only.
        // Subsequent init calls are idempotent — prefix is never mutated post-init
        // (INSERT OR IGNORE swallows the duplicate key).
        let p = prefix.unwrap_or("QP").to_string();
        validate_prefix(&p)?;
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES \
                ('project_uuid', ?1), \
                ('schema_version', ?2), \
                ('display_prefix', ?3)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), SCHEMA_VERSION, p],
        )?;
        // schema_version must advance even on existing stores being migrated forward.
        conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
            rusqlite::params![SCHEMA_VERSION],
        )?;
    }

    insert_default_tags(&conn, default_tags)?;

    Ok(conn)
}

pub fn default_tags(conn: &Connection) -> Result<Vec<String>> {
    let mut s = conn.prepare("SELECT name FROM default_tag ORDER BY name")?;
    let rows = s.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn insert_default_tags(conn: &Connection, tags: &[String]) -> Result<usize> {
    if tags.is_empty() {
        return Ok(0);
    }
    let mut total = 0;
    let mut stmt = conn.prepare("INSERT OR IGNORE INTO default_tag(name) VALUES (?)")?;
    for t in tags {
        if t.is_empty() {
            continue;
        }
        total += stmt.execute([t])?;
    }
    Ok(total)
}

/// Read the display-id prefix from the `meta` table. Defaults to `"QP"` if absent
/// (older databases predating the prefix work).
pub fn display_prefix(conn: &rusqlite::Connection) -> Result<String> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'display_prefix'",
            [],
            |r| r.get(0),
        )
        .ok();
    Ok(v.unwrap_or_else(|| "QP".to_string()))
}

/// Validate a user-supplied prefix: 2–5 ASCII uppercase letters.
pub fn validate_prefix(s: &str) -> Result<()> {
    let ok = (2..=5).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_uppercase());
    if !ok {
        return Err(constraint(format!(
            "invalid --prefix `{s}` (must be 2-5 uppercase ASCII letters)"
        )));
    }
    Ok(())
}

pub fn with_tx<T>(conn: &mut Connection, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    match f(&tx) {
        Ok(v) => {
            tx.commit()?;
            Ok(v)
        }
        Err(e) => {
            let _ = tx.rollback();
            Err(e)
        }
    }
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
        rusqlite::params![task_id, kind, agent_id, s],
    )?;
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
        [],
    )?;
    Ok(())
}

/// The single open assignment for a task, if any.
///
/// TODO(QP-68): migrates to store.rs in a later wave.
pub struct OpenAssignment {
    pub id: i64,
    pub agent_id: String,
    pub claimed_at: Option<String>,
}

/// The decided semantic for "who currently owns this task": latest-OPEN-row.
/// `ORDER BY id DESC LIMIT 1` is a defensive tiebreak — Slice A's guard against
/// more than one open assignment per task makes latest-open and latest-by-id
/// provably equivalent, so it should never actually need to fire.
pub fn current_assignment(tx: &Transaction, task_id: i64) -> Result<Option<OpenAssignment>> {
    tx.query_row(
        "SELECT id, agent_id, claimed_at FROM assignment
          WHERE task_id = ?1 AND completed_at IS NULL
          ORDER BY id DESC LIMIT 1",
        [task_id],
        |r| {
            Ok(OpenAssignment {
                id: r.get(0)?,
                agent_id: r.get(1)?,
                claimed_at: r.get(2)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Recursive check: would adding `from -depends_on-> to` create a cycle?
pub fn would_cycle(tx: &Transaction, from: i64, to: i64) -> Result<bool> {
    if from == to {
        return Ok(true);
    }
    // From `to`, can we reach `from` via depends_on edges? If yes → cycle.
    let n: i64 = tx.query_row(
        "WITH RECURSIVE reach(id) AS (
           SELECT depends_on_task_id FROM dep WHERE task_id = ?1
           UNION
           SELECT d.depends_on_task_id FROM dep d JOIN reach r ON r.id = d.task_id
         )
         SELECT COUNT(*) FROM reach WHERE id = ?2",
        [to, from],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_str_round_trip() {
        for s in [
            State::Pending,
            State::Ready,
            State::Assigned,
            State::Running,
            State::Done,
            State::Cancelled,
        ] {
            assert_eq!(State::from_str(s.as_str()), Some(s));
            assert_eq!(s.to_string(), s.as_str());
        }
        assert_eq!(State::from_str("nope"), None);
        assert_eq!(State::from_str("blocked"), None);
    }

    #[test]
    fn state_constants_alias_enum() {
        assert_eq!(STATE_PENDING, State::Pending.as_str());
        assert_eq!(STATE_READY, State::Ready.as_str());
        assert_eq!(STATE_ASSIGNED, State::Assigned.as_str());
        assert_eq!(STATE_RUNNING, State::Running.as_str());
        assert_eq!(STATE_DONE, State::Done.as_str());
        assert_eq!(STATE_CANCELLED, State::Cancelled.as_str());
    }
}

#[cfg(test)]
mod prefix_tests {
    use super::*;

    #[test]
    fn validate_prefix_accepts_2_to_5_upper() {
        for ok in ["QP", "QPU", "ACME", "ALPHA"] {
            assert!(validate_prefix(ok).is_ok(), "should accept {ok}");
        }
    }

    #[test]
    fn validate_prefix_rejects_bad() {
        for bad in ["", "Q", "TOOLONG", "qp", "Q1", "QP_", "QP-"] {
            assert!(validate_prefix(bad).is_err(), "should reject `{bad}`");
        }
    }

    #[test]
    fn display_prefix_defaults_to_qp() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let conn = open(&db).unwrap();
        assert_eq!(display_prefix(&conn).unwrap(), "QP");
    }

    #[test]
    fn display_prefix_honors_init_value() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let conn = open_with_prefix(&db, Some("ACME")).unwrap();
        assert_eq!(display_prefix(&conn).unwrap(), "ACME");
    }
}
