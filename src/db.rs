//! SQLite storage layer: connections, transactions, and migrations.
//!
#![doc = include_str!("../docs/modules/db.md")]

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::path::{Path, PathBuf};
use thiserror::Error;

const SCHEMA: &str = include_str!("schema.sql");

/// Typed state of a task in the workflow.
///
/// Use this anywhere the database schema's `state` column is read or written.
/// It is also the CLI-facing `--state` vocabulary (`clap::ValueEnum`) — one
/// definition for both surfaces, so they can't drift.
///
/// clap's default `ValueEnum` rendering is kebab-case, which for these
/// single-word variants is identical to `as_str()`'s lowercase spelling;
/// `state_enum_values_match_as_str` in the test module below pins that down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
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

// Legacy `&str` constants — kept as thin aliases over `State::*.as_str()` for
// `add.rs`, the only remaining call site that hasn't been converted to the
// typed `State` variant directly. The ASSIGNED/RUNNING/DONE/CANCELLED
// siblings were deleted in QP-118 once the SQL sweep left them unused.
pub const STATE_PENDING: &str = State::Pending.as_str();
pub const STATE_READY: &str = State::Ready.as_str();

/// The agent-facing error taxonomy.
///
/// `main` matches on the variant to pick an exit code and (in `--json` mode) a
/// `kind` string for the `{"error": {...}}` envelope.
///
/// Six buckets, deliberately — not one variant per failure site. The
/// *variant* is what a calling agent branches retry-vs-give-up on; the *code*
/// string inside `Conflict`/`Invariant` is for logs and precise skill
/// authoring, and can grow additively without breaking existing matchers.
///
/// The `--json` envelope carries a seventh `kind`, `internal`, which is not a
/// variant here: `main.rs` emits it for any error that does not downcast to
/// this type. Adding a variant means updating `kind()`, `exit_code()`,
/// `README.md`'s envelope table and the exit-code table in
/// `docs/architecture.md` together.
#[derive(Debug, Error)]
pub enum QuipuError {
    /// Wrong state / lost race — retrying may succeed once state settles. Exit 2.
    #[error("{message}")]
    Conflict {
        code: &'static str,
        message: String,
        task: Option<String>,
    },
    /// A different agent holds this — do not retry, escalate or reassign. Exit 2.
    #[error("{message}")]
    NotOwner {
        message: String,
        task: Option<String>,
        owner: Option<String>,
    },
    /// Referenced entity or edge does not exist. Exit 2.
    #[error("{message}")]
    NotFound {
        message: String,
        task: Option<String>,
    },
    /// Structural invariant violated (dependency cycle) — replan. Exit 2.
    #[error("{message}")]
    Invariant { code: &'static str, message: String },
    /// Bad CLI input — exit 1.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// No store here — this process is not attached to a project. Exit 6.
    ///
    /// Not `NotFound` (which is about a row inside a store that *is* open) and
    /// not `InvalidInput` (the arguments were fine; the machine is not set up).
    /// It gets its own exit code because it is the one failure a caller must
    /// never retry and must never read as "empty project".
    #[error("{message}")]
    NoStore {
        message: String,
        path: Option<String>,
    },
}

impl QuipuError {
    /// Stable lowercase discriminant for the `--json` error envelope's `kind` field.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Conflict { .. } => "conflict",
            Self::NotOwner { .. } => "not_owner",
            Self::NotFound { .. } => "not_found",
            Self::Invariant { .. } => "invariant",
            Self::InvalidInput(_) => "invalid_input",
            Self::NoStore { .. } => "no_store",
        }
    }

    /// Exit code contract, tabulated in `docs/architecture.md` under "Exit codes".
    ///
    /// 1 is "your input was wrong", 2 is "the store said no — retrying may
    /// succeed", 6 is "there is no store to say anything". 6 is deliberately
    /// not 2: a skill that retries on 2 would loop forever against a machine
    /// that will never grow a store on its own. 3/4 (`wait`) and 5
    /// (`remote doctor`) are exited directly by those commands and never
    /// reach this table.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::InvalidInput(_) => 1,
            Self::NoStore { .. } => 6,
            _ => 2,
        }
    }

    /// `{"error": {...}}` body for `--json` mode. Always rendered to stderr by the caller.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Conflict {
                code,
                message,
                task,
            } => serde_json::json!({
                "kind": self.kind(), "code": code, "message": message, "task": task,
            }),
            Self::NotOwner {
                message,
                task,
                owner,
            } => serde_json::json!({
                "kind": self.kind(), "message": message, "task": task, "owner": owner,
            }),
            Self::NotFound { message, task } => serde_json::json!({
                "kind": self.kind(), "message": message, "task": task,
            }),
            Self::Invariant { code, message } => serde_json::json!({
                "kind": self.kind(), "code": code, "message": message,
            }),
            Self::InvalidInput(message) => serde_json::json!({
                "kind": self.kind(), "message": message,
            }),
            Self::NoStore { message, path } => serde_json::json!({
                "kind": self.kind(), "message": message, "path": path,
            }),
        }
    }
}

/// Wrong state / lost race. `task` should be the canonical display_id when known.
pub fn conflict(code: &'static str, msg: impl Into<String>, task: Option<String>) -> anyhow::Error {
    anyhow::Error::from(QuipuError::Conflict {
        code,
        message: msg.into(),
        task,
    })
}

/// A different agent holds this task than the caller claims to be.
pub fn not_owner(
    msg: impl Into<String>,
    task: Option<String>,
    owner: Option<String>,
) -> anyhow::Error {
    anyhow::Error::from(QuipuError::NotOwner {
        message: msg.into(),
        task,
        owner,
    })
}

/// The error for a write that found no open assignment on the task.
///
/// Usually the plain `no_open_assignment` conflict. The exception is the one
/// that matters: when the caller's own hold was broken by a `reclaim`, it gets
/// told so by name instead — see `lease::dispossessed`.
pub fn no_open_assignment(
    tx: &Transaction,
    task_id: i64,
    display_id: &str,
    agent: Option<&str>,
    msg: impl Into<String>,
) -> Result<anyhow::Error> {
    if let Some(agent) = agent {
        if let Some(e) = crate::lease::dispossessed(tx, task_id, agent)? {
            return Ok(e);
        }
    }
    Ok(conflict(
        "no_open_assignment",
        msg,
        Some(display_id.to_string()),
    ))
}

/// Referenced entity or edge does not exist.
pub fn not_found(msg: impl Into<String>, task: Option<String>) -> anyhow::Error {
    anyhow::Error::from(QuipuError::NotFound {
        message: msg.into(),
        task,
    })
}

/// Structural invariant violated (e.g. dependency cycle).
pub fn invariant(code: &'static str, msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(QuipuError::Invariant {
        code,
        message: msg.into(),
    })
}

pub fn invalid_input(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(QuipuError::InvalidInput(msg.into()))
}

/// Nothing to work in: the store this command would have used does not exist.
pub fn no_store(msg: impl Into<String>, path: Option<String>) -> anyhow::Error {
    anyhow::Error::from(QuipuError::NoStore {
        message: msg.into(),
        path,
    })
}

/// Refuse to proceed unless a store is already on disk at `path`.
///
/// The backstop under `open`, so a command module cannot reintroduce
/// create-on-demand by calling `open` on a path nobody created. `main.rs`
/// checks first and produces a friendlier, cwd-aware message; by the time this
/// fires the caller reached `open` some other way, so it names the path.
pub fn require_store(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    Err(no_store(
        format!(
            "no quipu store at {}; run `qp init` to create one, or point --db/QP_DB at an existing store",
            path.display()
        ),
        Some(path.display().to_string()),
    ))
}

/// The attach check every command but `qp init` runs, before it opens anything.
///
/// `explicit` says whether the path came from `--db`/`QP_DB`, which changes the
/// advice but not the outcome: an explicit path at a file that is not there is
/// a misconfigured machine just as surely as an unconfigured cwd is, and it
/// must not be answered by creating the store the operator asked for by name.
pub fn require_attached(path: &Path, explicit: bool) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let message = if explicit {
        format!(
            "no quipu store at {} (from --db/QP_DB); run `qp init` with that path to create one, \
             or point it at an existing store",
            path.display()
        )
    } else {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string());
        format!(
            "no quipu store in {cwd} or any parent; run `qp init` here, or run from a project directory"
        )
    };
    Err(no_store(message, Some(path.display().to_string())))
}

/// Locates the store.
///
/// Explicit `--db`/`QP_DB` wins. Otherwise, in order:
///
/// 1. the nearest `.quipu/db.sqlite` walking up from the cwd, **stopping at
///    the checkout root** (the first ancestor holding a `.git` entry);
/// 2. the store beside the repository git-common-dir asks for;
/// 3. the nearest `.quipu/db.sqlite` *above* that checkout root;
/// 4. `./.quipu/db.sqlite`, so `qp init` has somewhere to create one.
///
/// Step 2 is what makes a bare `qp` work from inside a git worktree. A
/// worktree's checkout is a *sibling* of the main repo, not a descendant, so
/// the ancestor walk never reaches the main repo's `.quipu/`. Asking git for
/// `--git-common-dir` resolves to the main repo's `.git` from either a
/// worktree or the primary checkout, and the store is looked for next to it.
/// Without this, every agent working in a worktree would have to set `QP_DB`
/// by hand — and one that got it wrong would silently file into the wrong
/// project.
///
/// The bounded walk in step 1 is what lets step 2 actually run. An unbounded
/// walk climbs straight past a worktree into whatever directory happens to
/// hold it; if *anything* up there has a `.quipu/`, that store wins and the
/// git-aware fallback is never reached — a silent wrong-project write, since
/// the wrong store is still a valid store and reads and writes both succeed.
/// Bounding the walk at the checkout root keeps a deliberately nested store
/// (one inside the worktree) winning, which a plain "git first" reorder would
/// have broken, while a store belonging to some unrelated parent directory no
/// longer outranks the repository the cwd is actually in.
///
/// Step 3 preserves the pre-existing behaviour for a repo that has no store of
/// its own but sits under a directory that does: nothing in view conflicts, so
/// the outer store is still the answer.
///
/// git is only spawned when the checkout itself has no store, so the common
/// case — a repo with `.quipu/` at its root — costs no subprocess.
///
/// Never fails on a missing store: it answers "where *would* the store be",
/// which is a question with an answer even on a machine that has none. Whether
/// that path may be *created* is a separate and deliberate decision, made by
/// `require_attached`/`require_store` and taken only by `qp init` — because a
/// tool that creates on demand cannot tell an empty project from a
/// misconfigured machine, and answers both with exit 0.
pub fn resolve_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    let cwd = std::env::current_dir()?;

    // Walk up, tracking whether we have crossed out of the current checkout.
    // A store found at or below the checkout root wins outright; one found
    // above it is only a fallback, behind the git-aware lookup.
    let mut above_checkout: Option<PathBuf> = None;
    let mut inside_checkout = true;
    for a in cwd.ancestors() {
        let c = a.join(".quipu").join("db.sqlite");
        if c.exists() {
            if inside_checkout {
                return Ok(c);
            }
            above_checkout = Some(c);
            break;
        }
        // `.git` is a directory in a primary checkout and a file in a linked
        // worktree; either marks the root of the checkout we started in.
        if inside_checkout && a.join(".git").exists() {
            inside_checkout = false;
        }
    }

    if let Some(c) = git_common_dir_store(&cwd) {
        return Ok(c);
    }
    if let Some(c) = above_checkout {
        return Ok(c);
    }
    Ok(cwd.join(".quipu").join("db.sqlite"))
}

/// The store beside the repository's common git dir, if it exists.
///
/// `--git-common-dir` is the main repo's `.git` seen from anywhere in the
/// repository, linked worktrees included — which is the whole point.
fn git_common_dir_store(cwd: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let git_dir = PathBuf::from(&raw);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        cwd.join(git_dir)
    };
    let git_dir = git_dir.canonicalize().unwrap_or(git_dir);
    let c = git_dir.parent()?.join(".quipu").join("db.sqlite");
    c.exists().then_some(c)
}

/// Guard against filing into the wrong project.
///
/// Warns when an explicit `--db`/`QP_DB` points at a different store than the
/// one a bare `qp` would have picked.
///
/// The comparison runs through `resolve_path` itself rather than a private
/// ancestor walk, so the warning always describes the store the implicit path
/// would really have chosen. That matters in a worktree: a second, subtly
/// different walk here reported a mismatch on every single command for a
/// perfectly consistent `QP_DB`, and an always-on warning for a legitimate
/// configuration is one people learn to filter out. It now fires on a genuine
/// difference of `project_uuid` and nothing else.
///
/// Only fires when the path was given explicitly — that is, in automation. Which
/// is exactly why `json` matters: under `--json`, stderr is JSON Lines (zero or
/// more `{"warning":...}` objects, then at most one `{"error":...}`), so a prose
/// warning here would leave the consumer unable to parse the error that follows.
/// See QP-120.
pub fn warn_on_project_mismatch(explicit: &Option<PathBuf>, json: bool) -> Result<()> {
    let Some(explicit_path) = explicit else {
        return Ok(());
    };
    let local = resolve_path(None)?;
    if !local.exists() {
        return Ok(());
    }
    if local.canonicalize().ok() == explicit_path.canonicalize().ok() {
        return Ok(());
    }
    let uuid_explicit = read_project_uuid(explicit_path).ok().flatten();
    let uuid_local = read_project_uuid(&local).ok().flatten();
    if let (Some(a), Some(b)) = (uuid_explicit, uuid_local) {
        if a != b {
            if json {
                eprintln!(
                    "{}",
                    serde_json::json!({"warning": {
                        "kind": "project_uuid_mismatch",
                        "explicit_db": explicit_path.display().to_string(),
                        "explicit_uuid": a,
                        "cwd_db": local.display().to_string(),
                        "cwd_uuid": b,
                    }})
                );
            } else {
                eprintln!("warning: project_uuid mismatch — QP_DB={} (uuid {}) but cwd resolves to {} (uuid {})",
                    explicit_path.display(), a, local.display(), b);
            }
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
        .optional()?;
    Ok(v)
}

/// The schema version this binary was compiled against.
///
/// Bump it whenever `schema.sql` changes in a way that requires a migration.
/// The value is stamped into `meta(key='schema_version')` on first init;
/// subsequent `open()` calls compare it to decide whether to (re)apply DDL.
///
/// Bumping this is the ONLY thing that gets new DDL onto an existing store.
/// `migrate` skips the entire `execute_batch(SCHEMA)` when the stamped version
/// already matches, so an additive `CREATE ... IF NOT EXISTS` added to
/// `schema.sql` without a bump reaches freshly-initialised stores only and
/// silently never reaches anyone's real database. QP-142's partial unique index
/// took this from "2" to "3" for exactly that reason.
///
/// "4" adds `dep.mode`. A bump alone was still not enough there: re-running
/// `CREATE TABLE IF NOT EXISTS dep` is a no-op on a store that already has the
/// table, so the new column needs the explicit `ALTER` in
/// [`add_missing_columns`].
///
/// "5" adds the three tables `qp import` writes — `task_origin`,
/// `foreign_event` and `conflict`. New tables, not new columns, so
/// `execute_batch(SCHEMA)` is enough once the stamp moves; without the bump
/// they would reach freshly-initialised stores only, and the first real store
/// anyone tried to merge into would fail on `no such table`.
///
/// "7" adds the three claim-liveness columns on `assignment` —
/// `last_contact`, `lease_seconds` and `holder_host`. Columns on a table that
/// already exists, so like "4" the bump alone is not enough: each one needs an
/// entry in [`add_missing_columns`]. They are all nullable with no default,
/// which is what makes the migration inert — every pre-existing open claim
/// lands with no stated expiry, and a claim with no stated expiry never
/// expires.
pub const SCHEMA_VERSION: &str = "7";

/// Open the connection and set pragmas.
///
/// Shared by both the hot path (`open`) and init-time (`init`) — no migration,
/// no prefix handling.
fn open_conn(path: &Path) -> Result<Connection> {
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
    Ok(conn)
}

/// Apply the `ALTER TABLE` steps that `schema.sql` structurally cannot.
///
/// Every statement in `schema.sql` is `CREATE ... IF NOT EXISTS`, which is why
/// the file is safe to re-run — and also why it can only ever *add tables and
/// indexes*. A column added to an existing `CREATE TABLE` body reaches freshly
/// initialized stores and nobody else, silently, because the `IF NOT EXISTS`
/// makes the whole statement a no-op wherever the table is already there. Any
/// column added to a table that shipped in an earlier `SCHEMA_VERSION` needs a
/// matching entry here.
///
/// Runs on *every* open, not just on a version mismatch, and each step probes
/// `pragma_table_info` rather than trusting the stamp. Gating it on the version
/// would leave a store whose stamp is *ahead* of its schema permanently broken:
/// nothing would ever look at it again, every command would fail on the missing
/// column, and `qp init` — the obvious thing to try — would report success while
/// repairing nothing. Ungated, any command heals it. The cost is one indexed
/// pragma read per invocation, which is noise against the cold-start budget.
///
/// # Concurrency
///
/// The probe and the `ALTER` cannot be made atomic without holding the write
/// lock across both, so this does not try. SQLite has no
/// `ADD COLUMN IF NOT EXISTS`, and *tolerating* `duplicate column name` is the
/// idempotent form: every racing process reads "absent", one wins the lock, and
/// the losers observe that the column they wanted now exists. That is success,
/// not failure.
///
/// Getting this wrong is not theoretical. The first version probed and then
/// raised, so the first command run after an upgrade — which in a wave is
/// several processes at once — failed in all but one process, as an untyped
/// `internal` error no calling agent could retry on. Single-process testing
/// cannot see it.
fn add_missing_columns(conn: &Connection) -> Result<()> {
    // (table, column, full DDL fragment). SQLite requires an ADD COLUMN default
    // to be a constant, so `NOT NULL DEFAULT 'blocks'` is legal and backfills
    // every existing row in place.
    const ADDITIONS: &[(&str, &str, &str)] = &[
        ("dep", "mode", "mode TEXT NOT NULL DEFAULT 'blocks'"),
        // Nullable and defaultless on purpose: an open claim that predates
        // leases must migrate to "no stated expiry", which is unbreakable.
        // Backfilling `last_contact` with `now` would invent a lease nobody
        // took and hand a live claim an expiry date.
        ("assignment", "last_contact", "last_contact TEXT"),
        ("assignment", "lease_seconds", "lease_seconds INTEGER"),
        ("assignment", "holder_host", "holder_host TEXT"),
    ];
    for (table, column, ddl) in ADDITIONS {
        let present: bool = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2",
                rusqlite::params![table, column],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if present {
            continue;
        }
        match conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {ddl}")) {
            Ok(()) => {}
            // Someone else added it between the probe and here. Match on the
            // message because SQLite reports this as a generic `SQLITE_ERROR`
            // with no distinguishing extended code.
            Err(e) if e.to_string().contains("duplicate column name") => {}
            Err(e) => {
                return Err(anyhow::Error::from(e))
                    .with_context(|| format!("adding {table}.{column}"))
            }
        }
    }
    Ok(())
}

/// Bring the schema up to date, keyed off the stamped `schema_version`.
///
/// If the stamp matches `SCHEMA_VERSION`, DDL is up to date and we skip the
/// schema re-apply on the hot path (saves 1–3 ms per invocation on
/// already-initialized stores). If it is missing, this is either a fresh db or
/// a pre-versioning store — apply the DDL and stamp meta rows via
/// INSERT OR IGNORE (idempotent, no read-then-write).
///
/// On a genuinely fresh db, `meta` doesn't exist yet — that's a real
/// `no such table` error, not a "no rows" case, so `.optional()` alone
/// would propagate it and fail `init`. Check for the table first so a
/// missing table is treated the same as a missing row (fresh db), while
/// any other error on an existing table still propagates.
///
/// Returns the schema_version as it stood *before* this call (`None` means
/// fresh db or pre-versioning store), so callers that also care about the
/// pre-migration prefix (i.e. `init`'s mismatch warning) can reuse it
/// without a second round-trip.
///
/// Called from both `open()` (every command, so this MUST stay
/// self-healing — a user who upgrades the binary and runs a read command
/// before ever running `qp init` depends on this) and `init()`.
fn migrate(
    conn: &Connection,
    prefix: Option<&str>,
    project_key: Option<&str>,
) -> Result<Option<String>> {
    let meta_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    let current: Option<String> = if meta_exists {
        conn.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?
    } else {
        None
    };
    if current.as_deref() != Some(SCHEMA_VERSION) {
        conn.execute_batch(SCHEMA).context("applying schema")?;
        // Stamp project_uuid + schema_version + display_prefix + project_key on
        // first init only. Subsequent init calls are idempotent — prefix is never
        // mutated post-init (INSERT OR IGNORE swallows the duplicate key).
        let p = prefix.unwrap_or("QP").to_string();
        validate_prefix(&p)?;
        // The key goes in the same statement as the uuid so a fresh store can
        // never exist without one: a crash between two statements would leave a
        // store that silently looks pre-identity and gets a derived key it does
        // not deserve. A store that already exists keeps whatever it has —
        // `project::backfill` below is what decides for those.
        let key = match project_key {
            Some(k) => k.to_string(),
            None => crate::project::mint(),
        };
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES \
                ('project_uuid', ?1), \
                ('schema_version', ?2), \
                ('display_prefix', ?3), \
                ('project_key', ?4)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), SCHEMA_VERSION, p, key],
        )?;
        // schema_version must advance even on existing stores being migrated forward.
        conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
            rusqlite::params![SCHEMA_VERSION],
        )?;
    }
    // After the batch, so the tables exist on a fresh store — and outside the
    // version gate, so a store whose stamp is ahead of its schema still gets
    // healed instead of being skipped forever. See `add_missing_columns`.
    add_missing_columns(conn)?;
    // Ungated for the same reason: every store that predates project identity
    // is already stamped at a version this binary knows, so a gated backfill
    // would never reach one.
    crate::project::backfill(conn)?;
    // Ungated for the same reason, and NOT folded into the INSERT OR IGNORE
    // batch above: that statement ignores per row, so a store missing only the
    // name would be handed a value there and never reach the derivation that
    // knows what the name should be. See `project::backfill_name`.
    crate::project::backfill_name(conn)?;
    Ok(current)
}

/// The hot path: open + migrate.
///
/// No prefix handling, no default-tag seeding — those are init-time-only
/// concerns (see `init`). Every command but `init` calls this, and it must
/// remain migration-capable so a stale on-disk schema self-heals on the first
/// read command after a binary upgrade, rather than surfacing as a confusing
/// `no such table` / `no such column` error.
///
/// It opens; it does not create. `require_store` fires first, so a caller
/// pointed at a path with no store gets `no_store` (exit 6) instead of a fresh
/// empty database. `init` is the only creator, and reaches `open_conn`
/// directly.
pub fn open(path: &Path) -> Result<Connection> {
    require_store(path)?;
    let conn = open_conn(path)?;
    migrate(&conn, None, None)?;
    Ok(conn)
}

/// Open a store, applying the one-time bookkeeping only `qp init` needs.
///
/// Init-time concerns only: prefix validation (via `migrate`'s first-stamp
/// path), the prefix-mismatch warning, `--default-tag` seeding, and the project
/// identity a joining store inherits. Shares the actual DDL/version-check logic
/// with `open` via `migrate` — this is not a separate migration path.
///
/// `project_key` of `None` means "start a project": a fresh store mints its own.
/// `Some` means "join one", and is refused rather than applied if this store
/// already answers to a different key.
pub fn init(
    path: &Path,
    prefix: Option<&str>,
    default_tags: &[String],
    project_key: Option<&str>,
) -> Result<Connection> {
    let conn = open_conn(path)?;
    let current = migrate(&conn, prefix, project_key)?;
    // A store's project identity is set once, when the store is created. An
    // `init` that asks for a different one is a person about to file into a
    // project this store is not part of, so it fails rather than re-homing a
    // store whose tickets are already published under the old key.
    if let Some(want) = project_key {
        let have = crate::project::require(&conn)?;
        if have != want {
            return Err(conflict(
                "project_key_mismatch",
                format!(
                    "this store already belongs to project `{have}` and cannot join `{want}`. \
                     A store's project is fixed at creation; initialise a new store to join \
                     a different project."
                ),
                None,
            ));
        }
    }
    if let (Some(cur), Some(user_p)) = (current.as_deref(), prefix) {
        if cur == SCHEMA_VERSION {
            let stored: Option<String> = conn
                .query_row(
                    "SELECT value FROM meta WHERE key='display_prefix'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
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

/// Read the display-id prefix from the `meta` table.
///
/// Defaults to `"QP"` if absent (older databases predating the prefix work).
pub fn display_prefix(conn: &rusqlite::Connection) -> Result<String> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'display_prefix'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.unwrap_or_else(|| "QP".to_string()))
}

/// Validate a user-supplied prefix: 2–5 ASCII uppercase letters.
pub fn validate_prefix(s: &str) -> Result<()> {
    let ok = (2..=5).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_uppercase());
    if !ok {
        // Pre-existing behavior (exit 2): kept as `Conflict` rather than `InvalidInput`
        // so `init_with_invalid_prefix_errors` (exit code 2) doesn't regress.
        return Err(conflict(
            "invalid_prefix",
            format!("invalid --prefix `{s}` (must be 2-5 uppercase ASCII letters)"),
            None,
        ));
    }
    Ok(())
}

/// Runs `f` inside a `BEGIN IMMEDIATE` transaction.
///
/// The primitive every guarded state transition is built on.
///
/// `IMMEDIATE` is the whole point, and it is why two agents racing the same
/// ticket produce exactly one winner and one clean conflict. It takes the
/// write lock at `BEGIN`, before any statement runs. The default `DEFERRED`
/// would instead start read-only and try to upgrade at the first write, and a
/// writer that loses that upgrade gets `SQLITE_BUSY` *mid-transaction*, where
/// the busy handler cannot help it — retrying would mean replaying reads it
/// already made against a snapshot that has since moved. Locking up front
/// turns that unrecoverable mid-flight failure into a wait at the door: the
/// second agent blocks under the `busy_timeout` set in `open_conn`, then runs
/// its guarded `UPDATE ... WHERE state IN (...)` against committed state, sees
/// `changes() == 0`, and reports a typed `Conflict`. `tests/race.rs` asserts
/// exactly that shape — one exit-0 winner, N-1 exit-2 losers carrying a stable
/// error code, and a single `state_change` event on the task.
///
/// The rollback on the error path is what lets a command write freely before
/// checking its guard: events and side-effect rows inserted ahead of a
/// transition that turns out to be rejected leave no trace. A conflict is
/// therefore never half-recorded. Its `Result` is deliberately discarded —
/// SQLite has already unwound the transaction by the time a rollback can fail,
/// and surfacing that would mask the real error being returned.
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

/// Appends one row to the immutable event log.
///
/// Taking `&Transaction` rather than `&Connection` is the guarantee: an event
/// commits with the transition it describes, or neither lands. That is what
/// makes the log trustworthy as an audit trail — there is no transition
/// without its event, and no event for a transition that was rolled back. It
/// is also what `tests/race.rs` counts to prove a single claim landed, since
/// "one `state_change` event" and "one non-null `claimed_at`" are the same
/// fact written twice.
///
/// The schema allows a null `task_id` for store-scoped events; every current
/// caller passes a task.
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
    let id = tx.last_insert_rowid();
    // Renewal rides here because this is the one statement every write against
    // a task already passes through. It is also where a holder whose lease was
    // broken is refused, by name — see `lease::touch`.
    if let Some(t) = task_id {
        crate::lease::touch(tx, t, agent_id)?;
    }
    Ok(id)
}

/// Appends a `state_change` event naming both ends of the transition.
///
/// The `from` key is always written, and its three cases are deliberately
/// distinct:
///
/// - **absent** — the event predates this function. The origin was never
///   stored and is recoverable from nothing else in the log, so that stretch of
///   history stays one-ended. It is documented rather than backfilled: a
///   reconstruction would be a guess wearing an audit trail's clothes.
/// - **`null`** — there was no origin. Task creation is the only such case; a
///   task comes into existence already in a state.
/// - **a string** — a real transition, and the pair is enough to check the move
///   was legal without consulting the task table at all.
///
/// The key is spelled to match the `edit` event's per-field `{"from","to"}`
/// pairs, so one convention covers both and a consumer learns it once. No
/// schema version bump rides with it: `schema.sql` is unchanged, the payload is
/// opaque JSON either way, and a bump would advertise a migration that cannot
/// exist.
///
/// Callers pass the origin their guard already named. Where the guard is exact
/// (`WHERE state = 'running'`) that is a literal; where it names a set
/// (`WHERE state IN ('assigned','running')`) the caller reads the row first,
/// purely for the payload. That read is not a read-then-write — the `UPDATE`
/// stays conditional and `changes() == 1` remains the only thing that decides
/// whether the transition happened.
pub fn insert_state_change(
    tx: &Transaction,
    task_id: i64,
    from: Option<&str>,
    to: &str,
    agent: Option<&str>,
    extra: &[(&str, serde_json::Value)],
) -> Result<i64> {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "from".to_string(),
        match from {
            Some(s) => serde_json::Value::String(s.to_string()),
            None => serde_json::Value::Null,
        },
    );
    payload.insert("to".to_string(), serde_json::Value::String(to.to_string()));
    for (k, v) in extra {
        payload.insert((*k).to_string(), v.clone());
    }
    insert_event(
        tx,
        Some(task_id),
        "state_change",
        agent,
        Some(&serde_json::Value::Object(payload)),
    )
}

/// The state a task sits in right now, for stamping an event's `from`.
///
/// An auxiliary read for event quality, never for control flow — the same
/// permitted exception `abandon` and `reclaim` already take to name the state
/// they landed in. Inside the `IMMEDIATE` transaction no other writer can move
/// the row between this and the guarded `UPDATE` that follows, and if the guard
/// fails the transition did not happen no matter what this returned.
pub fn state_of(tx: &Transaction, task_id: i64) -> Result<Option<String>> {
    tx.query_row("SELECT state FROM task WHERE id = ?", [task_id], |r| {
        r.get(0)
    })
    .optional()
    .map_err(Into::into)
}

/// Re-derives readiness store-wide.
///
/// Every `pending` task whose dependencies are all resolved becomes `ready`.
///
/// The only edge into `ready`. No command promotes a task directly; they
/// change whatever they change and call this, which is why `abandon` and
/// `reclaim` release a claim by routing the task to `pending` and letting this
/// decide whether it deserves to come back — a task whose deps regressed while
/// it was held must not silently return to the ready pool.
///
/// "Resolved" means `done` *or* `cancelled`. Cancelling a blocker therefore
/// unblocks its dependents rather than stranding them, which is the behaviour
/// `cancel_terminates_task_unblocks_dependents` in `tests/cli.rs` pins down.
///
/// A task is also held back while anything containing it is blocked, which is
/// what [`FROZEN`] computes. Readiness is therefore no longer a local property
/// — a slice can be individually unblocked and still not workable.
///
/// Set-based and idempotent by construction: it sweeps the whole table, so
/// callers never work out *which* tasks a change might have unblocked, and
/// calling it twice is free. It only ever promotes — nothing here demotes a
/// task, so running it at the wrong moment costs a query, not correctness.
/// [`demote_frozen`] is the other half.
///
/// **Private, and that is the fix.** It emits nothing, so every promotion it
/// performs is invisible to `qp timeline` and unreachable by anything replaying
/// the log. It used to be `pub` and was called directly from six commands —
/// including `complete`, the most common write in the system — which is how the
/// log came to disagree with the task table about which work was ready. The
/// only caller now is [`refresh_ready_logged`], which wraps it in the diff that
/// produces the events, so a new call site cannot skip the log without first
/// deleting this paragraph.
fn refresh_ready(tx: &Transaction) -> Result<()> {
    tx.execute(
        &format!(
            "{FROZEN}
             UPDATE task
                SET state = 'ready'
              WHERE state = 'pending'
                AND NOT EXISTS (
                  SELECT 1 FROM dep d
                  JOIN task t2 ON t2.id = d.depends_on_task_id
                  WHERE d.task_id = task.id
                    AND t2.state NOT IN ('done','cancelled')
                )
                AND id NOT IN (SELECT id FROM frozen)"
        ),
        [],
    )?;
    Ok(())
}

/// Every task held back by a blocker on something that contains it.
///
/// A `WITH RECURSIVE` clause defining `frozen(id)`, meant to be prefixed to a
/// statement that then selects from it. It is written as a standalone set
/// rather than a correlated subquery on purpose: SQLite will not let a CTE
/// reference the row of an enclosing `UPDATE`, and computing the whole set once
/// keeps both callers set-based and idempotent, matching how every other
/// transition in this module works.
///
/// Two rules, and the second is the entire feature:
///
/// 1. A task with an unresolved *blocking* dep is frozen. This is the seed, and
///    it is redundant with the plain readiness check — the same tasks are held
///    back either way — but it is what the recursion needs to start from.
/// 2. Anything *contained* by a frozen task is frozen, transitively.
///
/// So a blocker attached to a wave freezes every slice inside it, at any depth,
/// while a blocker attached to one slice leaves its siblings alone. Where the
/// blocker is attached is the whole of the control, which is why there is no
/// setting to turn this off: a project-level toggle would make the same graph
/// mean different things in different stores, and a dashboard reading
/// `report --json` could not interpret an edge without also knowing the toggle.
///
/// Only `contains` edges are followed downward. Following `blocks` edges would
/// mean "X blocks the wave" also froze everything the wave merely waits on,
/// which is a different and unwanted claim.
///
/// **A terminal container freezes nothing.** Every node entering the set is
/// checked for `done`/`cancelled`, in both arms, so cancelling a wave releases
/// its slices and a cancelled sub-wave stops the freeze from passing through it.
/// Without that check, `qp cancel` on a container stranded its contents
/// permanently — the exact opposite of the rule [`refresh_ready`] states, where
/// cancelling a blocker unblocks its dependents rather than stranding them. The
/// readiness side had always honoured it; containment did not, and recovery
/// meant knowing to run `qp contains --rm` on a dead wave.
///
/// `UNION` rather than `UNION ALL` is load-bearing: it dedupes, so the walk
/// terminates even on a graph that somehow violated acyclicity. `UNION ALL`
/// would not.
pub const FROZEN: &str = "WITH RECURSIVE frozen(id) AS (
      SELECT d.task_id FROM dep d
        JOIN task b ON b.id = d.depends_on_task_id
        JOIN task c ON c.id = d.task_id
       WHERE d.mode = 'blocks'
         AND b.state NOT IN ('done','cancelled')
         AND c.state NOT IN ('done','cancelled')
      UNION
      SELECT d.depends_on_task_id FROM dep d
        JOIN frozen f ON f.id = d.task_id
        JOIN task ch ON ch.id = d.depends_on_task_id
       WHERE d.mode = 'contains'
         AND ch.state NOT IN ('done','cancelled')
    )";

/// Demote every `ready` task that is frozen by a blocker above it.
///
/// The counterpart to [`refresh_ready`], and the reason propagation needs a
/// demote at all: attaching a prerequisite to a wave has to reach slices that
/// are already sitting in the ready pool, or the freeze only applies to work
/// that had not been unblocked yet.
///
/// Matches `ready` only. `assigned` and `running` are deliberately out of
/// scope — a task already in an agent's hands is never yanked back, so a
/// freeze applied mid-wave stops the *next* slice from being dispatched rather
/// than interrupting one in flight. That is the same rule the single-edge
/// demote in [`link_dep`] follows, and it is why the ownership gate can be on
/// the container alone: no slice's owner can lose work to it.
///
/// Returns the ids it actually demoted — the rows whose guarded `UPDATE` matched
/// — not the candidates it considered. Inside the `IMMEDIATE` transaction the
/// two are the same set, but a caller reading the return value is asking what
/// changed, and the guard is what answers that.
pub fn demote_frozen(tx: &Transaction, agent: Option<&str>, via: &str) -> Result<Vec<i64>> {
    let candidates: Vec<i64> = {
        let mut stmt = tx.prepare(&format!(
            "{FROZEN} SELECT id FROM task WHERE state = 'ready' AND id IN (SELECT id FROM frozen)"
        ))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut demoted = Vec::new();
    for id in &candidates {
        // Guarded per row rather than one blanket UPDATE: the guard is what
        // makes this safe to call after any edge mutation, and `changes() == 1`
        // is the invariant the rest of this module holds to.
        let n = tx.execute(
            "UPDATE task SET state = ?1 WHERE id = ?2 AND state = ?3",
            rusqlite::params![State::Pending, id, State::Ready],
        )?;
        if n == 1 {
            demoted.push(*id);
            // `from` is a literal, not a read: the guard above matched only a
            // `ready` row, so the origin is settled by the `UPDATE` itself.
            // This is the transition the origin matters most for — a demotion
            // is otherwise indistinguishable in the log from a task arriving at
            // `pending` for the first time.
            insert_state_change(
                tx,
                *id,
                Some(State::Ready.as_str()),
                State::Pending.as_str(),
                agent,
                &[("via", serde_json::json!(via))],
            )?;
        }
    }
    Ok(demoted)
}

/// [`refresh_ready`], plus a `state_change` event for each task it promoted.
///
/// `refresh_ready` is a set-based sweep with no idea which rows it touched, so
/// the promotions it performs are invisible to `qp timeline`. Commands that
/// unblock work — anything removing an edge — owe the audit log an entry, and
/// this is how they get one: snapshot the candidates first, sweep, then diff.
///
/// The snapshot is the set of `pending` tasks with no unresolved deps *before*
/// the sweep — a superset of what actually gets promoted, since the sweep also
/// excludes anything frozen by a blocker on a container above it. Reading state
/// back afterwards rather than trusting the snapshot is what keeps the event log
/// honest across that gap, and the gap is why the read-back is not optional.
///
/// Returns the ids promoted, so a caller that cares whether one *particular*
/// task came back (`qp depends --rm` reports this) can check without a third
/// query.
pub fn refresh_ready_logged(tx: &Transaction, agent: Option<&str>, via: &str) -> Result<Vec<i64>> {
    let candidates: Vec<i64> = {
        let mut stmt = tx.prepare(
            "SELECT t.id FROM task t WHERE t.state = 'pending' \
               AND NOT EXISTS (SELECT 1 FROM dep d JOIN task t2 ON t2.id = d.depends_on_task_id \
                                WHERE d.task_id = t.id AND t2.state NOT IN ('done','cancelled'))",
        )?;
        let rows = stmt
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    refresh_ready(tx)?;
    let mut promoted = Vec::new();
    for tid in candidates {
        let now: String =
            tx.query_row("SELECT state FROM task WHERE id = ?", [tid], |r| r.get(0))?;
        if now == "ready" {
            promoted.push(tid);
            // Every candidate was `pending` when the snapshot was taken and the
            // sweep only ever promotes, so the origin is a literal here too.
            insert_state_change(
                tx,
                tid,
                Some(State::Pending.as_str()),
                State::Ready.as_str(),
                agent,
                &[("via", serde_json::json!(via))],
            )?;
        }
    }
    Ok(promoted)
}

/// The single open assignment for a task, if any.
///
/// TODO(QP-68): migrates to store.rs in a later wave.
pub struct OpenAssignment {
    pub id: i64,
    pub agent_id: String,
    pub claimed_at: Option<String>,
}

/// Who currently owns this task, defined as the latest open assignment row.
///
/// `ORDER BY id DESC LIMIT 1` is a defensive tiebreak — the guard against more
/// than one open assignment per task makes latest-open and latest-by-id
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

/// Would adding `from -depends_on-> to` close a cycle in the dep graph?
///
/// Acyclicity is the invariant that makes `refresh_ready` terminate and the
/// tree renderable; a cycle would leave every task in the loop permanently
/// `pending` with no error to explain it. So this is a check-before-write on
/// `&Transaction`, not `&Connection`: the reachability query and the `INSERT`
/// that follows it commit atomically, and a concurrent writer cannot slip an
/// edge in between them to make the answer stale. Callers reject on `true`
/// with an `Invariant` error rather than repairing the graph — the caller
/// asked for something structurally impossible and should replan.
///
/// Self-edges short-circuit because the recursive query walks *outward* from
/// `to` and would not otherwise see `from == to` as a loop.
/// `depends_rejects_self_cycle` and `depends_rejects_transitive_cycle` in
/// `tests/cli.rs` cover both arms.
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

/// A dep edge that says "this must finish before that can start".
///
/// Written by `qp depends`, and the mode every row predating `dep.mode` carries.
pub const MODE_BLOCKS: &str = "blocks";
/// A dep edge that says "that is part of this".
///
/// Written by `qp contains` and `qp add --part-of`. Waits exactly like
/// [`MODE_BLOCKS`] — a container is not ready while its contents are open —
/// but additionally propagates the container's blockers down to its contents.
pub const MODE_CONTAINS: &str = "contains";

/// Gate a dep-edge mutation on ownership of the task being mutated.
///
/// The gate is on the downstream task — the one *gaining or losing* the edge —
/// because that is the row this writes to; the upstream task is untouched, so
/// its owner has no say. The practical consequence: you may point an edge *at*
/// another agent's in-flight work, but you may not reach into that work and
/// hang a new blocker on it.
pub fn require_edge_owner(
    tx: &Transaction,
    task_id: i64,
    display_id: &str,
    agent: Option<&str>,
) -> Result<()> {
    let state: String = tx.query_row("SELECT state FROM task WHERE id = ?", [task_id], |r| {
        r.get(0)
    })?;
    if !matches!(state.as_str(), "assigned" | "running") {
        return Ok(());
    }
    let assignee: Option<String> = current_assignment(tx, task_id)?.map(|o| o.agent_id);
    match (agent, assignee.as_deref()) {
        (Some(want), Some(have)) if want == have => Ok(()),
        _ => Err(not_owner(
            format!("{display_id} is {state}; --as must match latest assignee"),
            Some(display_id.to_string()),
            assignee,
        )),
    }
}

/// Add one dep edge in `mode`, demoting the depender if the edge blocks it.
///
/// Shared by `qp depends` and `qp contains`, which differ only in argument
/// order and the mode they pass — the cycle check, the demote and the audit
/// event are identical, and must stay that way: a containment edge that skipped
/// the cycle check would strand every task in the loop as permanently
/// `pending`.
///
/// Re-linking an existing pair in a different mode reclassifies it in place
/// rather than erroring, which is how a store that expressed containment as
/// plain deps converts: re-run `qp contains` over the edges that meant
/// containment all along. Reclassifying is *not* state-neutral: it cannot change
/// whether the depender itself is blocked, since both modes wait, but it moves
/// tasks in and out of the frozen set below. Converting a wave that already has
/// a blocker demotes every ready slice on the spot.
///
/// Returns `false` when the edge already existed in this mode, i.e. the call did
/// not alter the graph. Callers report that as success; asking for an edge that
/// is already there is not an error.
///
/// Reconciliation runs either way. Gating it on `changed` looks right and is
/// not: re-running the identical command is the obvious way to repair a store
/// whose frozen set drifted, and skipping the sweep made that repair a silent
/// no-op — the freeze could only be fixed by writing some *other* edge.
pub fn link_dep(
    tx: &Transaction,
    task_id: i64,
    task_display: &str,
    on_id: i64,
    on_display: &str,
    mode: &str,
    agent: Option<&str>,
) -> Result<bool> {
    let changed = write_dep_edge(tx, task_id, task_display, on_id, on_display, mode, agent)?;
    reconcile_readiness(tx, agent, mode)?;
    Ok(changed)
}

/// Writes one dep edge and demotes the depender if the edge blocks it.
///
/// The half of [`link_dep`] that touches only the named pair. Split out for
/// callers that write several edges in one transaction: running the store-wide
/// sweeps after each edge means sweeping a half-written graph, which invents
/// transitions the store was never in. Anything calling this directly owes the
/// transaction one [`reconcile_readiness`] before it commits.
///
/// Event order here is the point. The `dep_added` is written immediately after
/// the `INSERT` and *before* the demote it causes, so a replayer applying the
/// log in `event.id` order never sees a task pushed back to `pending` with no
/// edge in the graph to justify it. The edge exists first, then the states it
/// moves move.
pub fn write_dep_edge(
    tx: &Transaction,
    task_id: i64,
    task_display: &str,
    on_id: i64,
    on_display: &str,
    mode: &str,
    agent: Option<&str>,
) -> Result<bool> {
    if would_cycle(tx, task_id, on_id)? {
        return Err(invariant(
            "dependency_cycle",
            format!(
                "cycle: {task_display} depends on {on_display} which (transitively) depends on {task_display}"
            ),
        ));
    }
    // The DO UPDATE's WHERE suppresses the write when the mode already matches,
    // so `changes()` is exactly "did this call alter the graph".
    let changed = tx.execute(
        "INSERT INTO dep(task_id, depends_on_task_id, mode) VALUES (?1, ?2, ?3)
           ON CONFLICT(task_id, depends_on_task_id)
           DO UPDATE SET mode = excluded.mode WHERE dep.mode <> excluded.mode",
        rusqlite::params![task_id, on_id, mode],
    )?;
    // One event kind for both modes, with `mode` in the payload. A separate
    // `contains_added` kind would mean a consumer filtering on `dep_added`
    // silently missed half the graph mutations.
    if changed != 0 {
        insert_event(
            tx,
            Some(task_id),
            "dep_added",
            agent,
            Some(&serde_json::json!({"on": on_display, "mode": mode})),
        )?;
    }
    // Guarded demote: matches only a `ready` task that now has an unresolved
    // dep. `assigned`/`running` are deliberately excluded — a task already in an
    // agent's hands is never yanked back, and the ownership gate above is what
    // makes that safe rather than merely lucky.
    let demoted = tx.execute(
        "UPDATE task SET state = ?1
          WHERE id = ?2 AND state = ?3
            AND EXISTS (SELECT 1 FROM dep d JOIN task t2 ON t2.id = d.depends_on_task_id
                         WHERE d.task_id = ?2 AND t2.state NOT IN ('done','cancelled'))",
        rusqlite::params![State::Pending, task_id, State::Ready],
    )?;
    if demoted == 1 {
        insert_state_change(
            tx,
            task_id,
            Some(State::Ready.as_str()),
            State::Pending.as_str(),
            agent,
            &[("via", serde_json::json!(mode))],
        )?;
    }
    Ok(changed != 0)
}

/// Re-derives readiness store-wide after an edge mutation, in both directions.
///
/// A new edge can freeze work *below* the task it names: a blocker hung on a
/// container reaches every slice inside it, and a slice newly attached to an
/// already-blocked container inherits the freeze. Both arrive here, so this is
/// the single place either needs handling.
///
/// Reclassifying can also *thaw* — dropping a `contains` edge to `blocks` takes
/// everything under it out of the frozen set — so both halves run, and they run
/// even when the edge write changed nothing, so that re-running a command is a
/// real repair rather than a silent no-op.
///
/// Both halves are the logged variants. An unlogged promotion is precisely the
/// hole that made the log unreplayable, and reclassifying an edge can thaw work
/// several containment levels away from anything the caller named.
///
/// Call this **once**, after every edge in the transaction is written. Calling
/// it per edge sweeps a graph that is still half-written, and the promotions it
/// then logs are transitions no committed state ever held.
pub fn reconcile_readiness(tx: &Transaction, agent: Option<&str>, via: &str) -> Result<()> {
    demote_frozen(tx, agent, via)?;
    refresh_ready_logged(tx, agent, via)?;
    Ok(())
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
    }

    #[test]
    fn state_enum_values_match_as_str() {
        use clap::ValueEnum;
        for s in State::value_variants() {
            let pv = s.to_possible_value().expect("no skipped variants");
            assert_eq!(
                pv.get_name(),
                s.as_str(),
                "clap spelling must match as_str for {s:?}"
            );
        }
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
        // `init`, not `open`: `open` no longer creates, so a store has to be
        // made deliberately before there is a prefix to default.
        let conn = init(&db, None, &[], None).unwrap();
        assert_eq!(display_prefix(&conn).unwrap(), "QP");
    }

    #[test]
    fn display_prefix_honors_init_value() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let conn = init(&db, Some("ACME"), &[], None).unwrap();
        assert_eq!(display_prefix(&conn).unwrap(), "ACME");
    }
}
