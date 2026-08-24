//! `qp export` — serialise the store to append-only JSONL batches on disk.
//!
#![doc = include_str!("../../docs/modules/export.md")]

use crate::{db, outcome::Outcome, time};
use anyhow::{Context, Result};
use clap::Args;
use rusqlite::Connection;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The export format's own version, distinct from the store's `schema_version`.
///
/// Bump only when a reader written against the previous number would
/// misinterpret a batch. Every batch header carries it.
pub const FORMAT: &str = "quipu-export";

/// `2` adds `project_key`: the identity a set of stores share.
///
/// A reader stuck on `1` sees no key at all and cannot tell a peer of its own
/// project from an unrelated project sharing the ledger — the exact
/// misinterpretation this number exists to stop, so it is a bump rather than a
/// silently-ignored new field. The cost is that an older `qp` refuses batches a
/// newer one wrote; upgrading every machine on a shared ledger is the price of
/// the ledger being shared.
pub const FORMAT_VERSION: u32 = 2;

#[derive(Args, Debug)]
pub struct ExportArgs {
    /// Ledger root. A subdirectory named after the store's `project_uuid` is created under it.
    #[arg(long, value_name = "DIR")]
    pub out: PathBuf,
    /// Emit JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

/// First line of every batch: the envelope.
///
/// Store identity (`project_uuid`, `display_prefix`) and project identity
/// (`project_key`) live here rather than in the log because neither is ever
/// mutated — they name the store and the project it belongs to instead of
/// recording a change to either.
#[derive(Serialize)]
struct Header {
    record: &'static str,
    format: &'static str,
    format_version: u32,
    schema_version: String,
    project_uuid: String,
    /// The project these events belong to. Shared by every machine of it, and
    /// different from `project_uuid`, which names only the store that wrote it.
    project_key: String,
    display_prefix: String,
    default_tags: Vec<String>,
    /// Local rowid -> canonical name, for every task this store did not mint.
    ///
    /// Absent when the store has never imported, which is why an older batch
    /// with no such key is read correctly rather than ambiguously: every task
    /// in it is native to the store named in the header.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    task_origins: Vec<OriginRecord>,
    first_event_id: i64,
    last_event_id: i64,
    event_count: usize,
    exported_at: String,
}

/// One event, carried as it sits in the `event` table.
///
/// `task_id` is the rowid; a reader wanting the human name composes it as
/// `<display_prefix>-<task_id>`, which is exactly how the store mints it.
#[derive(Serialize)]
struct EventRecord {
    record: &'static str,
    id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<i64>,
    ts: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_id: Option<String>,
    /// The stored payload, parsed. Absent when the column is NULL.
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<serde_json::Value>,
    /// Escape hatch for a payload that is not JSON, so no byte is ever dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_text: Option<String>,
}

/// One task whose canonical name is not the one its rowid implies.
///
/// The events in this batch carry `task_id` as the rowid it holds *here*; a
/// peer needs this to recognise the ticket it already knows under another
/// number. See `task_origin` in `schema.sql`.
#[derive(Serialize)]
struct OriginRecord {
    task_id: i64,
    origin_uuid: String,
    origin_task_id: i64,
}

/// What one export did, or did not, write.
///
/// Public because `qp sync` composes export rather than reimplementing it, and
/// reports the same numbers rather than counting its own.
#[derive(Serialize)]
pub struct ExportOutcome {
    pub project_uuid: String,
    pub project_key: String,
    pub dir: String,
    pub file: Option<String>,
    pub first_event_id: Option<i64>,
    pub last_event_id: Option<i64>,
    pub event_count: usize,
    pub format_version: u32,
    pub schema_version: String,
}

impl Outcome for ExportOutcome {
    fn human(&self) -> String {
        match (&self.file, self.first_event_id, self.last_event_id) {
            (Some(f), Some(lo), Some(hi)) => {
                let n = self.event_count;
                let plural = if n == 1 { "event" } else { "events" };
                format!("exported {n} {plural} ({lo}..{hi}) to {f}")
            }
            _ => format!("nothing to export; {} is already current", self.dir),
        }
    }
}

pub fn run(db_path: &Path, a: ExportArgs) -> Result<()> {
    let out = export(db_path, &a.out)?;
    crate::outcome::emit(a.json, &out)
}

/// Writes at most one batch under `out_root/<project_uuid>/` and reports it.
///
/// Split from [`run`] so a caller that is not the CLI — `qp sync` — gets the
/// batch and the numbers without a second copy of the serialiser.
pub fn export(db_path: &Path, out_root: &Path) -> Result<ExportOutcome> {
    let conn = db::open(db_path)?;
    let project_uuid = meta(&conn, "project_uuid")?
        .ok_or_else(|| db::invalid_input("store has no project_uuid; run `qp init` first"))?;
    let project_key = crate::project::require(&conn)?;
    let schema_version = meta(&conn, "schema_version")?.unwrap_or_default();
    let display_prefix = db::display_prefix(&conn)?;
    let default_tags = db::default_tags(&conn)?;

    let dir = out_root.join(&project_uuid);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating export directory {}", dir.display()))?;

    let watermark = watermark(&dir, &project_uuid)?;
    let events = read_events(&conn, watermark)?;

    let mut out = ExportOutcome {
        project_uuid: project_uuid.clone(),
        project_key: project_key.clone(),
        dir: dir.display().to_string(),
        file: None,
        first_event_id: None,
        last_event_id: None,
        event_count: events.len(),
        format_version: FORMAT_VERSION,
        schema_version: schema_version.clone(),
    };
    if events.is_empty() {
        return Ok(out);
    }

    let lo = events.first().unwrap().id;
    let hi = events.last().unwrap().id;
    let header = Header {
        record: "header",
        format: FORMAT,
        format_version: FORMAT_VERSION,
        schema_version,
        project_uuid: project_uuid.clone(),
        project_key: project_key.clone(),
        display_prefix,
        default_tags,
        task_origins: task_origins(&conn)?,
        first_event_id: lo,
        last_event_id: hi,
        event_count: events.len(),
        exported_at: time::now_rfc3339(),
    };
    let path = dir.join(batch_name(&project_uuid, lo, hi));
    write_batch(&path, &header, &events)?;

    out.file = Some(path.display().to_string());
    out.first_event_id = Some(lo);
    out.last_event_id = Some(hi);
    Ok(out)
}

/// The batch filename: origin store, then the event range it closes over.
///
/// Two stores can never mint the same name, which is what makes merging two
/// ledgers a set union of files rather than a three-way merge of one file.
pub fn batch_name(project_uuid: &str, lo: i64, hi: i64) -> String {
    format!("{project_uuid}-{lo:010}-{hi:010}.jsonl")
}

/// Highest event id already on disk for this store, or 0.
///
/// Derived from the immutable filenames rather than from a state file, so
/// there is nothing to fall out of sync with the batches themselves.
fn watermark(dir: &Path, project_uuid: &str) -> Result<i64> {
    let mut max = 0i64;
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(hi) = batch_range(name, project_uuid).map(|(_, hi)| hi) {
            max = max.max(hi);
        }
    }
    Ok(max)
}

/// Parses `<project_uuid>-<lo>-<hi>.jsonl` back into its range.
///
/// Anything else in the directory — another store's batches, a README, a
/// half-written temp file — is not this store's business and is skipped.
pub fn batch_range(name: &str, project_uuid: &str) -> Option<(i64, i64)> {
    let rest = name.strip_prefix(project_uuid)?.strip_prefix('-')?;
    let rest = rest.strip_suffix(".jsonl")?;
    let (lo, hi) = rest.split_once('-')?;
    Some((lo.parse().ok()?, hi.parse().ok()?))
}

/// Every row of `task_origin`, which is empty in a store that never imported.
fn task_origins(conn: &Connection) -> Result<Vec<OriginRecord>> {
    let mut stmt = conn
        .prepare("SELECT task_id, origin_uuid, origin_task_id FROM task_origin ORDER BY task_id")?;
    let rows = stmt.query_map([], |r| {
        Ok(OriginRecord {
            task_id: r.get(0)?,
            origin_uuid: r.get(1)?,
            origin_task_id: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?", [key], |r| r.get(0))
        .optional()?)
}

fn read_events(conn: &Connection, after: i64) -> Result<Vec<EventRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, ts, kind, agent_id, payload FROM event WHERE id > ? ORDER BY id",
    )?;
    let rows = stmt.query_map([after], |r| {
        let raw: Option<String> = r.get(5)?;
        let (payload, payload_text) = match raw {
            None => (None, None),
            Some(s) => match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(v) => (Some(v), None),
                Err(_) => (None, Some(s)),
            },
        };
        Ok(EventRecord {
            record: "event",
            id: r.get(0)?,
            task_id: r.get(1)?,
            ts: r.get(2)?,
            kind: r.get(3)?,
            agent_id: r.get(4)?,
            payload,
            payload_text,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Writes the batch through a temp file, then renames it into place.
///
/// A batch is immutable once named, so a partially written one must never
/// carry a name the watermark scan trusts — that would silently swallow every
/// event the crash lost.
fn write_batch(path: &Path, header: &Header, events: &[EventRecord]) -> Result<()> {
    let tmp = path.with_extension("jsonl.tmp");
    {
        let f =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        let mut w = std::io::BufWriter::new(f);
        writeln!(w, "{}", serde_json::to_string(header)?)?;
        for e in events {
            writeln!(w, "{}", serde_json::to_string(e)?)?;
        }
        w.flush()?;
        w.into_inner()?.sync_all()?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}
