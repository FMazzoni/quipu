//! `qp import` — rebuild or merge a store from the JSONL batches `qp export` writes.
//!
#![doc = include_str!("../../docs/modules/import.md")]

use crate::cmd::export::{batch_name, FORMAT, FORMAT_VERSION};
use crate::{db, id, merge as order, outcome::Outcome};
use anyhow::{Context, Result};
use clap::Args;
use rusqlite::Transaction;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The tag a merge puts on a task whose fields could not be reconciled.
///
/// Detection and surfacing are this command's job; the verb that settles a
/// conflict is not, so the marker has to be visible to `qp list` on its own.
pub const CONFLICT_TAG: &str = "needs-resolution";

/// What a person must be told when a ledger predates an authoritative log.
///
/// The restore succeeds and the task count looks right, which is exactly why
/// this has to be said out loud: the missing rows are missing because no event
/// was ever written for them, so there is nothing to detect and no anomaly to
/// raise. A silent 95%-complete backup is worse than a loud partial one.
const PRE_FIDELITY_WARNING: &str = "\
warning: this ledger contains history written before the event log recorded every
warning: write. In that era tag, dependency and description writes emitted NO event
warning: at all, so they are not in the ledger and cannot be restored — and nothing
warning: can enumerate what is missing, because an event never written leaves no
warning: trace. The task count can look right while the store is materially
warning: incomplete. Treat that stretch of history as partial, keep the original
warning: store if you still have it, and see `docs/modules/import.md`.";

/// The fields an `edit` event is allowed to name.
///
/// Whitelisted because the field name is interpolated into SQL, and because a
/// new editable field is a decision about merge semantics that should be made
/// deliberately rather than inherited.
const EDITABLE: &[&str] = &["title", "tier", "description"];

#[derive(Args, Debug)]
pub struct ImportArgs {
    /// Ledger directory: either a `<project_uuid>` batch directory or a root holding several.
    #[arg(long, value_name = "DIR")]
    pub from: PathBuf,
    /// Disaster recovery: adopt the ledger's identity and rebuild an EMPTY store from it.
    #[arg(long)]
    pub restore: bool,
    /// Sync: fold another store's batches into this one, rebuilding the projection.
    #[arg(long)]
    pub merge: bool,
    /// Accept a replay that produced anomalies instead of refusing to write.
    #[arg(long)]
    pub force: bool,
    /// Emit JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Restore,
    Merge,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Restore => "restore",
            Self::Merge => "merge",
        }
    }
}

// ---------------------------------------------------------------------------
// the ledger on disk
// ---------------------------------------------------------------------------

/// One batch header, carrying the envelope its events are interpreted under.
struct Header {
    project_uuid: String,
    /// The project the writing store belongs to, or `None` on a batch written
    /// before project identity existed. `project::effective_key` is the one
    /// place that decides what `None` means; never read this field raw.
    project_key: Option<String>,
    display_prefix: String,
    schema_version: String,
    default_tags: Vec<String>,
    task_origins: Vec<(i64, String, i64)>,
    last_event_id: i64,
}

/// One event as it sits in a batch, plus the store that minted it.
///
/// `(origin_uuid, origin_id)` is the event's name anywhere in the world, which
/// is what makes a union of overlapping batches a set rather than a stream.
#[derive(Clone)]
struct LedgerEvent {
    origin_uuid: String,
    origin_id: i64,
    task_id: Option<i64>,
    ts: String,
    kind: String,
    agent_id: Option<String>,
    payload: Option<String>,
}

/// Everything a `--from` directory holds, deduplicated.
struct Ledger {
    batches: usize,
    /// Batches skipped because they belong to another project on this ledger.
    foreign: usize,
    headers: Vec<Header>,
    events: BTreeMap<(String, i64), LedgerEvent>,
}

impl Ledger {
    /// The uuids of the stores that wrote these batches, in sorted order.
    fn uuids(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .headers
            .iter()
            .map(|h| h.project_uuid.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        v.sort();
        v
    }

    /// The last header written by `uuid`, which carries its freshest state.
    ///
    /// `schema_version`, the default-tag set and the origin map are store state
    /// snapshotted at export time rather than events, so the newest batch is
    /// the only one whose copy is current.
    fn newest_header(&self, uuid: &str) -> Option<&Header> {
        self.headers
            .iter()
            .filter(|h| h.project_uuid == uuid)
            .max_by_key(|h| h.last_event_id)
    }
}

/// Reads every batch under `root`, one directory level deep.
///
/// A `--from` pointing at a ledger root and one pointing at a single store's
/// batch directory both work, because the header names its own store and the
/// filename is checked against it rather than against the directory.
///
/// `scope` is the project key a batch must belong to. One ledger holds many
/// projects on purpose — they share a git repository and everyone fetches the
/// whole thing — so a merge that read every directory it found would fold every
/// other project's tickets into this store. That absorption was measured before
/// project identity existed. `None` reads everything, which is what a restore
/// wants: an empty store has no key yet to be scoped by.
///
/// The comparison is against `project::effective_key`, never against the raw
/// header field, so a keyless batch is matched as the store that wrote it
/// standing alone rather than as a wildcard that matches whoever asks.
///
/// Scoping happens before the filename check below, so a batch another project
/// hand-edited no longer refuses this store's merge. That is deliberate: a file
/// this store will never read is not this store's to police, and the alternative
/// is one project being able to wedge every other project's sync.
fn read_ledger(root: &Path, scope: Option<&str>) -> Result<Ledger> {
    let mut files = Vec::new();
    collect_batches(root, true, &mut files)?;
    files.sort();

    let mut ledger = Ledger {
        batches: 0,
        foreign: 0,
        headers: Vec::new(),
        events: BTreeMap::new(),
    };
    for path in files {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut lines = text.lines();
        let Some(first) = lines.next() else { continue };
        let Ok(raw) = serde_json::from_str::<Value>(first) else {
            continue; // not a batch; someone else's file in the directory
        };
        if raw.get("record").and_then(Value::as_str) != Some("header") {
            continue;
        }
        let header = parse_header(&raw, &path)?;
        if let Some(scope) = scope {
            let key =
                crate::project::effective_key(header.project_key.as_deref(), &header.project_uuid);
            if key != scope {
                ledger.foreign += 1;
                continue;
            }
        }

        // The filename is what the exporter's high-water scan trusts, so a
        // batch whose name and header disagree is a renamed or hand-edited
        // file. Reading it anyway would import a range nothing can re-derive.
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let expected = batch_name(
            &header.project_uuid,
            first_event_id(&raw, &path)?,
            header.last_event_id,
        );
        if name != expected {
            return Err(db::invalid_input(format!(
                "{}: batch is named `{name}` but its header says `{expected}`. A batch \
                 is immutable and named by the range it closes over; a mismatch means \
                 the file was renamed or rewritten.",
                path.display()
            )));
        }

        for line in lines {
            let v: Value = serde_json::from_str(line)
                .with_context(|| format!("parsing a record in {}", path.display()))?;
            if v.get("record").and_then(Value::as_str) != Some("event") {
                continue;
            }
            let ev = parse_event(&v, &header.project_uuid, &path)?;
            // Overlapping batches are legal — deleting one makes the next
            // export re-emit its range — so the first copy of an event wins
            // and every later copy is dropped.
            ledger
                .events
                .entry((ev.origin_uuid.clone(), ev.origin_id))
                .or_insert(ev);
        }
        ledger.headers.push(header);
        ledger.batches += 1;
    }
    Ok(ledger)
}

/// Gathers `*.jsonl` paths from `dir`, descending one level when `deep`.
fn collect_batches(dir: &Path, deep: bool, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading ledger {}", dir.display()))?;
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            if deep {
                collect_batches(&path, false, out)?;
            }
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

fn parse_header(raw: &Value, path: &Path) -> Result<Header> {
    // Read the version before anything else: a reader that interprets an
    // unknown layout under the rules it happens to know misreads it silently.
    if raw.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(db::invalid_input(format!(
            "{}: not a quipu export (`format` is {:?}, expected {FORMAT:?})",
            path.display(),
            raw.get("format")
        )));
    }
    let version = raw
        .get("format_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            db::invalid_input(format!("{}: header has no format_version", path.display()))
        })?;
    if version > FORMAT_VERSION as u64 {
        return Err(db::invalid_input(format!(
            "{}: batch is format_version {version}, but this binary reads at most \
             {FORMAT_VERSION}. Upgrade qp rather than reading it under the old rules.",
            path.display()
        )));
    }
    let s = |key: &str| -> Result<String> {
        raw.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| db::invalid_input(format!("{}: header has no `{key}`", path.display())))
    };
    let task_origins = raw
        .get("task_origins")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    Some((
                        r.get("task_id")?.as_i64()?,
                        r.get("origin_uuid")?.as_str()?.to_string(),
                        r.get("origin_task_id")?.as_i64()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Header {
        project_uuid: s("project_uuid")?,
        project_key: raw
            .get("project_key")
            .and_then(Value::as_str)
            .filter(|k| !k.is_empty())
            .map(str::to_string),
        display_prefix: s("display_prefix")?,
        schema_version: s("schema_version").unwrap_or_default(),
        default_tags: raw
            .get("default_tags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        task_origins,
        last_event_id: raw
            .get("last_event_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                db::invalid_input(format!("{}: header has no last_event_id", path.display()))
            })?,
    })
}

fn first_event_id(raw: &Value, path: &Path) -> Result<i64> {
    raw.get("first_event_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            db::invalid_input(format!("{}: header has no first_event_id", path.display()))
        })
}

fn parse_event(v: &Value, origin_uuid: &str, path: &Path) -> Result<LedgerEvent> {
    let origin_id = v.get("id").and_then(Value::as_i64).ok_or_else(|| {
        db::invalid_input(format!("{}: an event record has no id", path.display()))
    })?;
    let ts = v
        .get("ts")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            db::invalid_input(format!("{}: event {origin_id} has no ts", path.display()))
        })?
        .to_string();
    let kind = v
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            db::invalid_input(format!("{}: event {origin_id} has no kind", path.display()))
        })?
        .to_string();
    // `payload_text` is the exporter's escape hatch for a payload that was not
    // JSON. Carrying it back verbatim is what keeps the round trip lossless.
    let payload = match v.get("payload_text").and_then(Value::as_str) {
        Some(t) => Some(t.to_string()),
        None => v.get("payload").map(ToString::to_string),
    };
    Ok(LedgerEvent {
        origin_uuid: origin_uuid.to_string(),
        origin_id,
        task_id: v.get("task_id").and_then(Value::as_i64),
        ts,
        kind,
        agent_id: v
            .get("agent_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        payload,
    })
}

// ---------------------------------------------------------------------------
// the command
// ---------------------------------------------------------------------------

/// What one import did to the store.
///
/// Public because `qp sync` composes import rather than reimplementing it, and
/// reports the merge's own numbers rather than counting its own.
#[derive(Serialize)]
pub struct ImportOutcome {
    pub mode: &'static str,
    pub from: String,
    pub batches: usize,
    /// Batches on this ledger that belong to another project and were left
    /// alone. Non-zero is the normal, healthy shape of a shared ledger.
    #[serde(default)]
    pub foreign_batches: usize,
    pub stores: Vec<String>,
    pub events_read: usize,
    pub events_new: usize,
    /// Events a best-effort restore could not write, because they name a task
    /// the log cannot rebuild and `event.task_id` is a foreign key.
    pub events_lost: usize,
    /// Transitions replayed with a weaker check than the live command ran,
    /// because the event predates the log recording an origin state.
    pub unguarded_transitions: usize,
    /// Field values recovered from an edit's own `from`, where the creation
    /// event that should have carried them did not.
    pub repaired_fields: usize,
    /// Live rows the merged log never mentioned, kept rather than rebuilt away.
    #[serde(default)]
    pub rows_carried_forward: usize,
    /// Whether the ledger contains history from before the log recorded every
    /// write. If so the restore is partial in ways nothing can enumerate.
    pub pre_fidelity_history: bool,
    /// Whether the projection was written despite anomalies. A script that
    /// cares about a clean rebuild reads this rather than the exit code: the
    /// pre-fidelity restore path is a success at what it can do, not a failure.
    pub best_effort: bool,
    pub tasks: usize,
    pub conflicts: usize,
    pub conflicted_tasks: Vec<String>,
    /// Canonical names of the tasks no creation event in the ledger minted.
    pub unreconstructable_tasks: Vec<String>,
    /// Tasks a forced merge kept as they were rather than dropping.
    pub carried_over: Vec<String>,
    /// Event kinds replay derived no projection from, as `kind×count`.
    ///
    /// Almost always annotations, which have no projection to derive — but a
    /// structural kind from a newer quipu would land here too, and there the
    /// empty projection is a real gap. Named so a reader can tell which.
    #[serde(default)]
    pub opaque_kinds: Vec<String>,
    pub anomalies: Vec<String>,
}

impl Outcome for ImportOutcome {
    fn human(&self) -> String {
        let mut s = match self.mode {
            "restore" => format!(
                "restored {} events into {} tasks from {}",
                self.events_new, self.tasks, self.from
            ),
            _ => format!(
                "merged {} new events from {} store(s); {} tasks",
                self.events_new,
                self.stores.len(),
                self.tasks
            ),
        };
        if self.foreign_batches > 0 {
            s.push_str(&format!(
                "\n{} batch(es) belonging to other projects on this ledger were left alone.",
                self.foreign_batches
            ));
        }
        if self.best_effort {
            s.push_str(&format!(
                "\nWRITTEN DESPITE {} anomal{}, each one reported above. This rebuild is \
                 partial: read them before trusting what is here.",
                self.anomalies.len(),
                if self.anomalies.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            ));
        }
        if self.conflicts > 0 {
            s.push_str(&format!(
                "\n{} field conflict(s) need resolution on {} — tagged `{CONFLICT_TAG}`",
                self.conflicts,
                self.conflicted_tasks.join(", ")
            ));
        }
        if !self.unreconstructable_tasks.is_empty() {
            s.push_str(&format!(
                "\nBEST EFFORT: {} task(s) predate the event log being authoritative and \
                 cannot be rebuilt from it — {}.",
                self.unreconstructable_tasks.len(),
                self.unreconstructable_tasks.join(", ")
            ));
        }
        if self.unguarded_transitions > 0 {
            s.push_str(&format!(
                "\n{} transition(s) were replayed without a real guard: they predate the log \
                 recording an origin state, so there is nothing to check them against.",
                self.unguarded_transitions
            ));
        }
        if self.rows_carried_forward > 0 {
            s.push_str(&format!(
                "\n{} live row(s) the merged log never mentioned were kept as they were, \
                 rather than rebuilt away.",
                self.rows_carried_forward
            ));
        }
        if self.repaired_fields > 0 {
            s.push_str(&format!(
                "\n{} field value(s) were recovered from an edit's own `from`, because the \
                 creation event that should have carried them did not.",
                self.repaired_fields
            ));
        }
        if !self.opaque_kinds.is_empty() {
            s.push_str(&format!(
                "\n{} event kind(s) replay derives no projection from were carried verbatim: \
                 {}. Free-form `qp log` kinds are expected here; a kind a newer quipu writes \
                 as a real change is not, and would leave this projection short.",
                self.opaque_kinds.len(),
                self.opaque_kinds.join(", ")
            ));
        }
        if self.events_lost > 0 {
            s.push_str(&format!(
                "\n{} event(s) were not restored; they are listed above and the ledger \
                 files still hold them.",
                self.events_lost
            ));
        }
        if !self.carried_over.is_empty() {
            s.push_str(&format!(
                "\n{} task(s) carried across unchanged: {}.",
                self.carried_over.len(),
                self.carried_over.join(", ")
            ));
        }
        s
    }
}

pub fn run(db_path: &Path, a: ImportArgs) -> Result<()> {
    let out = import(db_path, &a)?;
    crate::outcome::emit(a.json, &out)
}

/// Reads `a.from`, replays it into the store, and reports what changed.
///
/// Split from [`run`] so `qp sync` composes the merge instead of shelling out
/// to a second `qp`, and so the warnings below are emitted once, here, for
/// every caller rather than only for the CLI one.
pub fn import(db_path: &Path, a: &ImportArgs) -> Result<ImportOutcome> {
    let mode = match (a.restore, a.merge) {
        (true, false) => Mode::Restore,
        (false, true) => Mode::Merge,
        _ => {
            return Err(db::invalid_input(
                "qp import needs exactly one of `--restore` (empty store, adopt the \
                 ledger's identity) or `--merge` (populated store, fold the ledger in). \
                 The mode decides what happens to identity and is never inferred.",
            ))
        }
    };
    let mut conn = db::open(db_path)?;
    // A merge writes into a store that already has an identity, so the project
    // to scope by is never in doubt and is never guessed: it is this store's
    // own key, read from the one place that answers that question. A restore
    // reads everything, because the store it is rebuilding into is empty and
    // has no key yet — the ledger is what tells it which project it is.
    let scope = match mode {
        Mode::Merge => Some(crate::project::require(&conn)?),
        Mode::Restore => None,
    };
    let ledger = read_ledger(&a.from, scope.as_deref())?;
    let stores = ledger.uuids();
    if ledger.events.is_empty() {
        return Ok(ImportOutcome {
            mode: mode.as_str(),
            from: a.from.display().to_string(),
            batches: ledger.batches,
            foreign_batches: ledger.foreign,
            stores,
            events_read: 0,
            events_new: 0,
            events_lost: 0,
            unguarded_transitions: 0,
            repaired_fields: 0,
            rows_carried_forward: 0,
            pre_fidelity_history: false,
            best_effort: false,
            tasks: 0,
            conflicts: 0,
            conflicted_tasks: Vec::new(),
            unreconstructable_tasks: Vec::new(),
            carried_over: Vec::new(),
            opaque_kinds: Vec::new(),
            anomalies: Vec::new(),
        });
    }

    let mut out = db::with_tx(&mut conn, |tx| match mode {
        Mode::Restore => restore(tx, a, &ledger, &stores),
        Mode::Merge => merge(tx, a, &ledger, &stores),
    })?;
    out.from = a.from.display().to_string();
    out.batches = ledger.batches;
    out.foreign_batches = ledger.foreign;
    out.events_read = ledger.events.len();

    for line in &out.anomalies {
        eprintln!("import anomaly: {line}");
    }
    // Said on stderr as well as in the outcome, because the anomalies are here
    // and a notice about them printed somewhere else is a notice nobody reads
    // next to the thing it is about.
    if out.best_effort && !a.force {
        eprintln!(
            "note: this ledger records no origin state for some transitions, so `--restore` \n\
             applied BEST EFFORT rather than refusing — a store rebuilt as far as the log \n\
             allows is worth more mid-recovery than an empty one. Every anomaly is above. \n\
             `--merge` still refuses on any anomaly: there it would damage live data."
        );
    }
    if out.pre_fidelity_history {
        eprintln!("{}", PRE_FIDELITY_WARNING);
    }
    Ok(out)
}

/// Refuses the write when replay hit anomalies — where refusing protects
/// something.
///
/// Returns whether it wrote anyway despite them, so the caller can say so.
///
/// A rebuild that produced an illegal sequence is a claim that the log and the
/// projection disagree, and committing it quietly is the silent-loss failure
/// this whole design rejects. But "refuse" is only the safer answer where there
/// is something on the other side of the refusal to protect, and that differs
/// by mode:
///
/// **Merge** refuses. It folds unjustifiable history into a store that holds
/// good data right now, and the store it would damage is the one in front of
/// you. Refusing costs a sync; accepting costs the data.
///
/// **Restore into an empty store** does not, when the ledger has already
/// declared itself pre-fidelity — some `state_change` in it records no origin
/// state, so `PRE_FIDELITY_WARNING` is going to print regardless. There is
/// nothing to protect: the target is empty by definition, and refusing hands
/// someone mid-recovery an empty store instead of a reported partial one. The
/// partial one is strictly better, and every anomaly is still reported in full.
///
/// The line is where the log's own claims are: legality is enforced absolutely
/// wherever the log says it knows the origin state, and relaxed only where the
/// log itself admits it does not. So a MODERN ledger that produces anomalies
/// still refuses in both modes — that is a real integrity signal about history
/// we do trust, and it stays hard.
///
/// `--force` is not the answer to the restore case and never was: a flag every
/// real recovery has to pass is a flag people learn to pass always, which
/// spends the guard's credibility everywhere else to buy it here.
fn guard_anomalies(mode: Mode, force: bool, summary: &Summary, lost_events: usize) -> Result<bool> {
    if summary.anomalies.is_empty() {
        return Ok(false);
    }
    if force || (mode == Mode::Restore && summary.unguarded > 0) {
        return Ok(true);
    }
    Err(db::invariant(
        "replay_anomaly",
        format!(
            "{} refused: replay produced {} anomal{}, and nothing was written.\n{}\n\
             Every anomaly, in full:\n  {}",
            mode.as_str(),
            summary.anomalies.len(),
            if summary.anomalies.len() == 1 {
                "y"
            } else {
                "ies"
            },
            unrebuildable_note(mode, summary, lost_events),
            summary.anomalies.join("\n  ")
        ),
    ))
}

/// The diagnosis for the one anomaly a person can neither fix nor wait out.
///
/// A ledger written before the event log became authoritative holds writes that
/// changed the store without recording an event. Replay cannot invent the task
/// those writes created, so it reports it — and the raw `FOREIGN KEY constraint
/// failed` that used to come out instead named nothing and helped no one, least
/// of all someone restoring a backup because something had already gone wrong.
fn unrebuildable_note(mode: Mode, summary: &Summary, lost_events: usize) -> String {
    if summary.unreconstructable.is_empty() {
        return "Re-run with `--force` to write the projection anyway; every anomaly is \
                still reported when you do."
            .to_string();
    }
    let named: Vec<String> = summary
        .unreconstructable
        .iter()
        .map(|(uuid, id)| format!("{uuid}/{id}"))
        .collect();
    let options = match mode {
        Mode::Restore => format!(
            "  --force     restore everything else — every other task, edge and event,\n\
             \x20             minus the {lost_events} event(s) naming those tasks, each one listed.\n\
             \x20 do nothing  the ledger files are unharmed and still hold the whole history."
        ),
        Mode::Merge => "  --force     merge everything else, and carry those task rows across\n\
             \x20             unchanged rather than dropping them: their current state is\n\
             \x20             real data even though their history is not rebuildable.\n\
             \x20 do nothing  this store is untouched."
            .to_string(),
    };
    format!(
        "{} task(s) are named by events that no creation event ever minted:\n  {}\n\n\
         That is history written before the event log became authoritative: some writes\n\
         changed the store without recording an event, so the origin state was never\n\
         stored. It cannot be backfilled — a reconstruction would be a guess wearing an\n\
         audit trail's clothes — so this is permanent for that stretch of history rather\n\
         than a bug awaiting a fix. See `docs/modules/import.md`.\n\nOptions:\n{}",
        named.len(),
        named.join("\n  "),
        options
    )
}

// ---------------------------------------------------------------------------
// restore
// ---------------------------------------------------------------------------

/// Rebuilds an empty store as the one the ledger came from, identity included.
///
/// The envelope lands before the events do: the projection mints display ids
/// out of `meta`, so a restore that replayed first would name every task after
/// the wrong store.
fn restore(
    tx: &Transaction,
    a: &ImportArgs,
    ledger: &Ledger,
    stores: &[String],
) -> Result<ImportOutcome> {
    if stores.len() != 1 {
        return Err(db::invalid_input(format!(
            "`--restore` rebuilds ONE store and {} are present in {} ({}). Point \
             `--from` at a single `<project_uuid>` directory, then `--merge` the rest \
             back in.",
            stores.len(),
            a.from.display(),
            stores.join(", ")
        )));
    }
    for table in ["task", "event", "dep", "tag", "assignment", "relation"] {
        let n: i64 = tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
        if n != 0 {
            return Err(db::conflict(
                "store_not_empty",
                format!(
                    "`--restore` writes into an EMPTY store and `{table}` holds {n} row(s). \
                     Restoring over live data would overwrite an identity someone is already \
                     filing against; use `--merge` to fold this ledger into what is here."
                ),
                None,
            ));
        }
    }

    let uuid = &stores[0];
    let header = ledger
        .newest_header(uuid)
        .expect("a uuid in `stores` came from a header");
    // A keyless batch is not an unknown project: it is the store that wrote it,
    // standing alone. Restoring one under a freshly minted key would quietly
    // rename the project every one of its already-published batches belongs to.
    let project_key = crate::project::effective_key(header.project_key.as_deref(), uuid);
    tx.execute(
        "INSERT INTO meta(key, value) VALUES ('project_uuid', ?1), ('display_prefix', ?2),
                                             ('schema_version', ?3), ('project_key', ?4)
           ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![
            uuid,
            header.display_prefix,
            header.schema_version,
            project_key
        ],
    )?;
    for t in &header.default_tags {
        tx.execute("INSERT OR IGNORE INTO default_tag(name) VALUES (?)", [t])?;
    }
    for (task_id, origin_uuid, origin_task_id) in &header.task_origins {
        tx.execute(
            "INSERT OR REPLACE INTO task_origin(task_id, origin_uuid, origin_task_id)
             VALUES (?,?,?)",
            rusqlite::params![task_id, origin_uuid, origin_task_id],
        )?;
    }

    // Restore is the one mode where the ledger IS this store's own log, so the
    // events go back into `event` under their original ids and the next export
    // resumes exactly where the old one stopped.
    let mut events: Vec<&LedgerEvent> = ledger.events.values().collect();
    events.sort_by_key(|e| e.origin_id);

    let mut replay = Replay::new(tx, &header.display_prefix, uuid, Mode::Restore)?;
    replay.load_origins(ledger);
    replay.index_creations(events.iter().copied());
    for ev in &events {
        replay.apply(ev)?;
    }
    let mut summary = replay.finish()?;

    // `event.task_id` is a foreign key, so an event naming a task the log never
    // created cannot be written at all. Partitioning BEFORE any insert is what
    // keeps that a diagnosis instead of a raw `FOREIGN KEY constraint failed`
    // from four frames down — the guard below gets to speak first.
    let alive = live_task_ids(tx)?;
    let (writable, lost): (Vec<&&LedgerEvent>, Vec<&&LedgerEvent>) = events
        .iter()
        .partition(|ev| ev.task_id.is_none_or(|t| alive.contains(&t)));

    let best_effort = guard_anomalies(Mode::Restore, a.force, &summary, lost.len())?;

    for ev in &lost {
        summary.anomalies.push(format!(
            "event {}/{} (`{}`) was NOT restored: it names task {} which the log cannot \
             rebuild. The ledger file still holds it.",
            ev.origin_uuid,
            ev.origin_id,
            ev.kind,
            ev.task_id.unwrap_or_default()
        ));
    }
    for ev in &writable {
        tx.execute(
            "INSERT INTO event(id, task_id, ts, kind, agent_id, payload) VALUES (?,?,?,?,?,?)",
            rusqlite::params![
                ev.origin_id,
                ev.task_id,
                ev.ts,
                ev.kind,
                ev.agent_id,
                ev.payload
            ],
        )?;
    }

    Ok(ImportOutcome {
        mode: "restore",
        from: String::new(),
        batches: 0,
        foreign_batches: 0,
        stores: stores.to_vec(),
        events_read: 0,
        events_new: writable.len(),
        events_lost: lost.len(),
        unguarded_transitions: summary.unguarded,
        repaired_fields: summary.repaired,
        rows_carried_forward: 0,
        pre_fidelity_history: summary.unguarded > 0,
        best_effort,
        tasks: summary.tasks,
        conflicts: summary.conflicts,
        opaque_kinds: opaque_kinds(&summary),
        unreconstructable_tasks: named(&summary),
        conflicted_tasks: summary.conflicted_tasks,
        carried_over: Vec::new(),
        anomalies: summary.anomalies,
    })
}

/// The rowids `task` actually holds right now.
fn live_task_ids(tx: &Transaction) -> Result<HashSet<i64>> {
    let mut stmt = tx.prepare("SELECT id FROM task")?;
    let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
    let mut out = HashSet::new();
    for row in rows {
        out.insert(row?);
    }
    Ok(out)
}

/// The canonical names of the tasks the log could not rebuild, for the outcome.
fn named(summary: &Summary) -> Vec<String> {
    summary
        .unreconstructable
        .iter()
        .map(|(uuid, id)| format!("{uuid}/{id}"))
        .collect()
}

/// The kinds replay projected nothing from, rendered for the outcome.
fn opaque_kinds(summary: &Summary) -> Vec<String> {
    summary
        .opaque
        .iter()
        .map(|(kind, n)| format!("{kind}\u{d7}{n}"))
        .collect()
}

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

/// Folds other stores' batches in and rebuilds the projection from the union.
///
/// Rebuilding rather than appending is what makes the result independent of the
/// order batches arrived in: the merged log is a SET, replayed under one total
/// order, so two machines that have seen the same batches agree whatever
/// sequence each of them saw first.
fn merge(
    tx: &Transaction,
    a: &ImportArgs,
    ledger: &Ledger,
    stores: &[String],
) -> Result<ImportOutcome> {
    let local_uuid: String = tx.query_row(
        "SELECT value FROM meta WHERE key = 'project_uuid'",
        [],
        |r| r.get(0),
    )?;
    let prefix = db::display_prefix(tx)?;

    // Our own batches coming back to us carry events we already hold under the
    // same names. Recording them again would be a second identity for one
    // event; a store that has genuinely lost its log wants `--restore`.
    let mut events_new = 0usize;
    for ev in ledger.events.values() {
        if ev.origin_uuid == local_uuid {
            continue;
        }
        events_new += tx.execute(
            "INSERT OR IGNORE INTO foreign_event(origin_uuid, origin_id, task_id, ts, kind,
                                                 agent_id, payload)
             VALUES (?,?,?,?,?,?,?)",
            rusqlite::params![
                ev.origin_uuid,
                ev.origin_id,
                ev.task_id,
                ev.ts,
                ev.kind,
                ev.agent_id,
                ev.payload
            ],
        )?;
    }

    // The headers explain the rowids in the events we just stored, and they are
    // not in reach next time — see `peer_task_origin` in `schema.sql`.
    for h in &ledger.headers {
        if h.project_uuid == local_uuid {
            continue;
        }
        for (store_task_id, origin_uuid, origin_task_id) in &h.task_origins {
            tx.execute(
                "INSERT OR REPLACE INTO peer_task_origin(store_uuid, store_task_id,
                                                         origin_uuid, origin_task_id)
                 VALUES (?,?,?,?)",
                rusqlite::params![h.project_uuid, store_task_id, origin_uuid, origin_task_id],
            )?;
        }
    }

    let merged = merged_log(tx, &local_uuid)?;

    // `event.task_id` points at `task`, and the rebuild below deletes every
    // task row before reinserting it under the same id. Deferring the check to
    // COMMIT is what lets that happen — and keeps it a real check: if the
    // merged log fails to recreate a task some event names, the commit fails.
    tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
    snapshot_projection(tx)?;

    // Children first: every projection table points at `task`.
    for table in ["tag", "dep", "relation", "assignment", "conflict"] {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
    }
    tx.execute("DELETE FROM task", [])?;

    let mut replay = Replay::new(tx, &prefix, &local_uuid, Mode::Merge)?;
    replay.load_origins(ledger);
    replay.index_creations(merged.iter());
    for ev in &merged {
        replay.apply(ev)?;
    }
    let mut summary = replay.finish()?;

    let best_effort = guard_anomalies(Mode::Merge, a.force, &summary, 0)?;

    // Under `--force`: a task the log cannot rebuild still has a real row, and
    // dropping it would destroy live data on every sync — a far worse outcome
    // than the lost history already is. Carry it across untouched and say so.
    let carried = carry_over_unrebuilt(tx, &prefix)?;
    for task in &carried {
        summary.anomalies.push(format!(
            "{task} was carried across UNCHANGED: the merged log cannot rebuild it, so its \
             row is the one that was already here rather than a replayed one."
        ));
    }
    let carried_rows = carry_forward_unlogged(tx, &summary)?;
    let tasks: i64 = tx.query_row("SELECT COUNT(*) FROM task", [], |r| r.get(0))?;

    Ok(ImportOutcome {
        mode: "merge",
        from: String::new(),
        batches: 0,
        foreign_batches: 0,
        stores: stores.to_vec(),
        events_read: 0,
        events_new,
        events_lost: 0,
        unguarded_transitions: summary.unguarded,
        repaired_fields: summary.repaired,
        rows_carried_forward: carried_rows,
        pre_fidelity_history: summary.unguarded > 0,
        best_effort,
        tasks: tasks as usize,
        conflicts: summary.conflicts,
        opaque_kinds: opaque_kinds(&summary),
        unreconstructable_tasks: named(&summary),
        conflicted_tasks: summary.conflicted_tasks,
        carried_over: carried,
        anomalies: summary.anomalies,
    })
}

/// Puts back every live row the merged log has nothing to say about.
///
/// A rebuild is only authoritative over what the log records. A store written
/// before the log recorded every write holds tags, edges, descriptions and
/// states that no event ever mentioned — the large majority of tag rows in a
/// real store measured against — and a merge that rebuilt from the log alone would
/// delete every one of them, silently, on every sync. That is not a partial
/// restore, it is data loss caused by syncing.
///
/// The distinction that makes this safe is between a row the log DELETED and a
/// row it never knew: replay records every `(task, tag)`, `(task, dep)`,
/// `(from, to, kind)` and `(task, field)` any event named, in either direction.
/// A row the log spoke to is the rebuild's business; a row it never named is
/// carried forward untouched. So a logged `tag_removed` still removes, and a
/// peer's edits still win, while unlogged history survives.
///
/// This is NOT gated behind `--force`. Not destroying live data is the default.
fn carry_forward_unlogged(tx: &Transaction, summary: &Summary) -> Result<usize> {
    tx.execute_batch(
        "DROP TABLE IF EXISTS temp.said_tag;
         DROP TABLE IF EXISTS temp.said_dep;
         DROP TABLE IF EXISTS temp.said_relation;
         DROP TABLE IF EXISTS temp.said_field;
         DROP TABLE IF EXISTS temp.moved;
         CREATE TEMP TABLE said_tag(task_id INTEGER, name TEXT);
         CREATE TEMP TABLE said_dep(task_id INTEGER, on_id INTEGER);
         CREATE TEMP TABLE said_relation(a INTEGER, b INTEGER, kind TEXT);
         CREATE TEMP TABLE said_field(task_id INTEGER, field TEXT);
         CREATE TEMP TABLE moved(task_id INTEGER);",
    )?;
    for (task, name) in &summary.said_tags {
        tx.execute(
            "INSERT INTO said_tag VALUES (?,?)",
            rusqlite::params![task, name],
        )?;
    }
    for (task, on) in &summary.said_deps {
        tx.execute(
            "INSERT INTO said_dep VALUES (?,?)",
            rusqlite::params![task, on],
        )?;
    }
    for (a, b, kind) in &summary.said_relations {
        tx.execute(
            "INSERT INTO said_relation VALUES (?,?,?)",
            rusqlite::params![a, b, kind],
        )?;
    }
    for (task, field) in &summary.said_fields {
        tx.execute(
            "INSERT INTO said_field VALUES (?,?)",
            rusqlite::params![task, field],
        )?;
    }
    for task in &summary.moved {
        tx.execute("INSERT INTO moved VALUES (?)", [task])?;
    }

    let before = changed_rows(tx)?;
    tx.execute_batch(
        "INSERT OR IGNORE INTO main.tag SELECT * FROM snap_tag s
           WHERE s.task_id IN (SELECT id FROM main.task)
             AND NOT EXISTS (SELECT 1 FROM said_tag m
                              WHERE m.task_id = s.task_id AND m.name = s.name);
         INSERT OR IGNORE INTO main.dep SELECT * FROM snap_dep s
           WHERE s.task_id IN (SELECT id FROM main.task)
             AND s.depends_on_task_id IN (SELECT id FROM main.task)
             AND NOT EXISTS (SELECT 1 FROM said_dep m
                              WHERE m.task_id = s.task_id AND m.on_id = s.depends_on_task_id);
         INSERT OR IGNORE INTO main.relation SELECT * FROM snap_relation s
           WHERE s.from_task_id IN (SELECT id FROM main.task)
             AND s.to_task_id IN (SELECT id FROM main.task)
             AND NOT EXISTS (SELECT 1 FROM said_relation m
                              WHERE m.a = s.from_task_id AND m.b = s.to_task_id
                                AND m.kind = s.kind);",
    )?;
    // Fields, one statement each: a column the log never set must keep the
    // value the store already had rather than being blanked by the rebuild.
    for field in ["tier", "description"] {
        tx.execute_batch(&format!(
            "UPDATE main.task SET {field} = (SELECT s.{field} FROM snap_task s WHERE s.id = main.task.id)
               WHERE EXISTS (SELECT 1 FROM snap_task s WHERE s.id = main.task.id
                              AND s.{field} IS NOT NULL)
                 AND NOT EXISTS (SELECT 1 FROM said_field m
                                  WHERE m.task_id = main.task.id AND m.field = '{field}');"
        ))?;
    }
    // State, on the same principle: only a transition that named its origin
    // gives the log authority to move a task the store already placed.
    tx.execute_batch(
        "UPDATE main.task SET state = (SELECT s.state FROM snap_task s WHERE s.id = main.task.id)
           WHERE EXISTS (SELECT 1 FROM snap_task s WHERE s.id = main.task.id
                          AND s.state <> main.task.state)
             AND id NOT IN (SELECT task_id FROM moved);",
    )?;
    Ok(changed_rows(tx)?.saturating_sub(before))
}

/// Rows in the projection tables, for measuring what a carry-forward put back.
fn changed_rows(tx: &Transaction) -> Result<usize> {
    let n: i64 = tx.query_row(
        "SELECT (SELECT COUNT(*) FROM tag) + (SELECT COUNT(*) FROM dep)
              + (SELECT COUNT(*) FROM relation)",
        [],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// Copies the projection aside before the rebuild wipes it.
///
/// Temp tables, so they vanish with the connection and can never be mistaken
/// for part of the store. They exist for one reason: what the merged log fails
/// to recreate is not automatically garbage.
fn snapshot_projection(tx: &Transaction) -> Result<()> {
    for table in ["task", "tag", "dep", "relation", "assignment"] {
        tx.execute_batch(&format!(
            "DROP TABLE IF EXISTS temp.snap_{table};
             CREATE TEMP TABLE snap_{table} AS SELECT * FROM main.{table};"
        ))?;
    }
    Ok(())
}

/// Reinstates the rows the rebuild could not produce, and names them.
///
/// Only edges with both endpoints present come back, so nothing is reinstated
/// into a dangling reference. Runs after the guard, which means it runs only
/// when the caller passed `--force` or there was nothing wrong in the first
/// place — in which case it finds nothing and does nothing.
fn carry_over_unrebuilt(tx: &Transaction, prefix: &str) -> Result<Vec<String>> {
    tx.execute_batch(
        "DROP TABLE IF EXISTS temp.orphan;
         CREATE TEMP TABLE orphan AS
             SELECT id FROM snap_task WHERE id NOT IN (SELECT id FROM main.task);",
    )?;
    let orphans: Vec<i64> = {
        let mut stmt = tx.prepare("SELECT id FROM orphan ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    if orphans.is_empty() {
        return Ok(Vec::new());
    }
    tx.execute_batch(
        "INSERT INTO main.task SELECT * FROM snap_task WHERE id IN (SELECT id FROM orphan);
         INSERT OR IGNORE INTO main.tag SELECT * FROM snap_tag
             WHERE task_id IN (SELECT id FROM orphan);
         INSERT OR IGNORE INTO main.dep SELECT * FROM snap_dep
             WHERE (task_id IN (SELECT id FROM orphan)
                    OR depends_on_task_id IN (SELECT id FROM orphan))
               AND task_id IN (SELECT id FROM main.task)
               AND depends_on_task_id IN (SELECT id FROM main.task);
         INSERT OR IGNORE INTO main.relation SELECT * FROM snap_relation
             WHERE (from_task_id IN (SELECT id FROM orphan)
                    OR to_task_id IN (SELECT id FROM orphan))
               AND from_task_id IN (SELECT id FROM main.task)
               AND to_task_id IN (SELECT id FROM main.task);
         INSERT INTO main.assignment SELECT * FROM snap_assignment
             WHERE task_id IN (SELECT id FROM orphan);",
    )?;
    Ok(orphans.iter().map(|id| id::encode(*id, prefix)).collect())
}

/// Every event this store knows about, in the one order all machines agree on.
///
/// The order itself is `merge`'s, not this function's: a union of batches is a
/// set and replay needs a sequence, and every consumer of that sequence has to
/// derive it the same way or convergence is a coincidence. All this adds is the
/// union of the local log with the peers' events, which is the part that needs
/// a database handle.
fn merged_log(tx: &Transaction, local_uuid: &str) -> Result<Vec<LedgerEvent>> {
    let mut out = Vec::new();
    let mut stmt = tx.prepare(
        "SELECT ?1 AS origin_uuid, id, task_id, ts, kind, agent_id, payload FROM event
         UNION ALL
         SELECT origin_uuid, origin_id, task_id, ts, kind, agent_id, payload FROM foreign_event",
    )?;
    let rows = stmt.query_map([local_uuid], |r| {
        Ok(LedgerEvent {
            origin_uuid: r.get(0)?,
            origin_id: r.get(1)?,
            task_id: r.get(2)?,
            ts: r.get(3)?,
            kind: r.get(4)?,
            agent_id: r.get(5)?,
            payload: r.get(6)?,
        })
    })?;
    for row in rows {
        out.push(row?);
    }
    order::sort_for_replay(&mut out, |e| {
        order::MergeKey::new(&e.ts, &e.origin_uuid, e.origin_id)
    });
    Ok(out)
}

// ---------------------------------------------------------------------------
// replay
// ---------------------------------------------------------------------------

/// One side of a field that two stores edited concurrently.
///
/// `from` is retained because it is the detector: two edits carrying the same
/// prior value are branching off one state, which is what concurrency IS.
#[derive(Clone)]
struct Branch {
    from: Option<String>,
    value: Option<String>,
    origin_uuid: String,
    origin_id: i64,
    ts: String,
}

/// What a replay pass produced, beyond the rows it wrote.
struct Summary {
    tasks: usize,
    conflicts: usize,
    conflicted_tasks: Vec<String>,
    anomalies: Vec<String>,
    /// Kinds carried verbatim with no projection derived, and how many of each.
    opaque: BTreeMap<String, usize>,
    /// Transitions applied without a real guard, because the event records no
    /// origin state to check them against.
    unguarded: usize,
    said_tags: HashSet<(i64, String)>,
    said_deps: HashSet<(i64, i64)>,
    said_relations: HashSet<(i64, i64, String)>,
    said_fields: HashSet<(i64, String)>,
    moved: HashSet<i64>,
    /// Field values taken from an edit's own `from`, because the creation event
    /// that should have carried them predates the log recording them.
    repaired: usize,
    /// Tasks named by an event that no creation event in the log ever minted.
    ///
    /// Its own field rather than one anomaly among many, because it is the one
    /// failure with a cause a person can act on and a fix that does not exist.
    unreconstructable: BTreeSet<(String, i64)>,
}

/// Projects a sequence of events onto the store, guarding every transition.
///
/// The guard is the same conditional `UPDATE ... WHERE state = <from>` plus
/// `changes() == 1` that the live commands use, so a merged log that implies an
/// impossible move (`pending` straight to `assigned`) is reported instead of
/// being written. That check is what makes ordering by wall clock defensible.
struct Replay<'a> {
    tx: &'a Transaction<'a>,
    /// Which import is running. A refusal costs a sync in one mode and a
    /// recovered store in the other, so the two do not weigh it the same.
    mode: Mode,
    prefix: String,
    local_uuid: String,
    /// Canonical `(origin_uuid, origin_task_id)` -> local rowid.
    tasks: HashMap<(String, i64), i64>,
    /// Per store, its local rowid -> the canonical name it stands for.
    origins: HashMap<(String, i64), (String, i64)>,
    next_id: i64,
    field_value: HashMap<(i64, String), Option<String>>,
    last_edit: HashMap<(i64, String), Branch>,
    conflict_base: HashMap<(i64, String), Option<String>>,
    branches: HashMap<(i64, String), Vec<Branch>>,
    anomalies: Vec<String>,
    /// Kinds replay derived no projection from, and how many of each.
    opaque: BTreeMap<String, usize>,
    unguarded: usize,
    /// Creation events, by the task they mint, so a forward reference can pull
    /// one forward instead of failing.
    creations: HashMap<(String, i64), LedgerEvent>,
    hoisted: HashSet<(String, i64)>,
    repaired: usize,
    /// Everything the merged log SPEAKS TO, so a merge can tell a row the log
    /// deliberately removed from a row it never knew about. See
    /// `carry_forward_unlogged`.
    said_tags: HashSet<(i64, String)>,
    said_deps: HashSet<(i64, i64)>,
    said_relations: HashSet<(i64, i64, String)>,
    said_fields: HashSet<(i64, String)>,
    moved: HashSet<i64>,
    unreconstructable: BTreeSet<(String, i64)>,
    /// Per task, the state event that last moved it: which store wrote it, its
    /// id there, and when. A refusal is a question about two events, not one —
    /// usually "whose clock is wrong?" — so the loser has to be able to name
    /// the winner.
    last_state: HashMap<i64, (String, i64, String)>,
}

impl<'a> Replay<'a> {
    fn new(tx: &'a Transaction<'a>, prefix: &str, local_uuid: &str, mode: Mode) -> Result<Self> {
        // The mint has to clear every id the store has ever used, not just the
        // live ones: a task deleted from the projection by a rebuild still owns
        // its name in the log, and reusing it would rename someone's ticket.
        let high: i64 = tx.query_row(
            "SELECT MAX(n) FROM (SELECT COALESCE(MAX(id), 0) AS n FROM task
                                 UNION ALL SELECT COALESCE(MAX(task_id), 0) FROM task_origin
                                 UNION ALL SELECT COALESCE(MAX(task_id), 0) FROM event
                                 UNION ALL SELECT COALESCE(MAX(seq), 0) FROM sqlite_sequence
                                            WHERE name = 'task')",
            [],
            |r| r.get(0),
        )?;
        let mut tasks = HashMap::new();
        let mut origins = HashMap::new();
        {
            let mut stmt =
                tx.prepare("SELECT task_id, origin_uuid, origin_task_id FROM task_origin")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (local, uuid, origin) = row?;
                tasks.insert((uuid.clone(), origin), local);
                // The map read in BOTH directions. Our own events reference an
                // imported ticket by the rowid it holds here, and no batch we
                // are reading need carry a header that says what that means —
                // it is our table, and this is the only copy of it in reach.
                origins.insert((local_uuid.to_string(), local), (uuid, origin));
            }
        }
        {
            let mut stmt = tx.prepare(
                "SELECT store_uuid, store_task_id, origin_uuid, origin_task_id
                   FROM peer_task_origin",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (store, store_task, uuid, origin) = row?;
                origins.insert((store, store_task), (uuid, origin));
            }
        }
        Ok(Self {
            tx,
            mode,
            prefix: prefix.to_string(),
            local_uuid: local_uuid.to_string(),
            tasks,
            origins,
            next_id: high + 1,
            field_value: HashMap::new(),
            last_edit: HashMap::new(),
            conflict_base: HashMap::new(),
            branches: HashMap::new(),
            anomalies: Vec::new(),
            opaque: BTreeMap::new(),
            unguarded: 0,
            last_state: HashMap::new(),
            creations: HashMap::new(),
            hoisted: HashSet::new(),
            repaired: 0,
            said_tags: HashSet::new(),
            said_deps: HashSet::new(),
            said_relations: HashSet::new(),
            said_fields: HashSet::new(),
            moved: HashSet::new(),
            unreconstructable: BTreeSet::new(),
        })
    }

    /// Indexes every creation event before replay starts.
    ///
    /// One command writes several events in one transaction, and
    /// `qp add --part-of` emits the containment edge BEFORE the creation of the
    /// task it points at. In id order that is a forward reference, and refusing it
    /// would condemn a store for the order two rows of one commit went in.
    fn index_creations<'e>(&mut self, events: impl Iterator<Item = &'e LedgerEvent>) {
        for ev in events {
            let Some(task) = ev.task_id else { continue };
            let payload: Value = ev
                .payload
                .as_deref()
                .and_then(|p| serde_json::from_str(p).ok())
                .unwrap_or(Value::Null);
            if !is_creation(&ev.kind, &payload) {
                continue;
            }
            let canon = self.canonical(&ev.origin_uuid, task);
            self.creations.entry(canon).or_insert_with(|| ev.clone());
        }
    }

    /// The local rowid of a canonical task.
    ///
    /// Pulls its creation forward if the log holds one replay has not reached.
    fn ensure_task(&mut self, canon: &(String, i64)) -> Result<Option<i64>> {
        if let Some(id) = self.tasks.get(canon) {
            return Ok(Some(*id));
        }
        let Some(ev) = self.creations.get(canon).cloned() else {
            return Ok(None);
        };
        let payload: Value = ev
            .payload
            .as_deref()
            .and_then(|p| serde_json::from_str(p).ok())
            .unwrap_or(Value::Null);
        // Marked AFTER the insert, not before: `create` treats a hoisted canon
        // as "already done" so the event's own turn is a no-op, and setting the
        // flag first would make this very call the no-op.
        self.create(&ev, &payload, canon.clone())?;
        self.hoisted.insert(canon.clone());
        Ok(self.tasks.get(canon).copied())
    }

    /// Learns the origin maps the batch headers carry.
    ///
    /// Without them a peer's events are unreadable in the one case that matters:
    /// a ticket it imported from us and then edited references it by ITS rowid.
    fn load_origins(&mut self, ledger: &Ledger) {
        for h in &ledger.headers {
            for (local, origin_uuid, origin_task_id) in &h.task_origins {
                self.origins.insert(
                    (h.project_uuid.clone(), *local),
                    (origin_uuid.clone(), *origin_task_id),
                );
            }
        }
    }

    /// The globally unique name of a rowid as seen from the store that wrote it.
    fn canonical(&self, origin_uuid: &str, rowid: i64) -> (String, i64) {
        self.origins
            .get(&(origin_uuid.to_string(), rowid))
            .cloned()
            .unwrap_or_else(|| (origin_uuid.to_string(), rowid))
    }

    /// The local rowid a canonical task holds here, minting one if it is new.
    ///
    /// A task native to this store keeps its own rowid, which is what makes a
    /// merge leave every display id anyone has already written down intact.
    fn mint(&mut self, canon: (String, i64)) -> Result<i64> {
        if let Some(id) = self.tasks.get(&canon) {
            return Ok(*id);
        }
        let local = if canon.0 == self.local_uuid {
            canon.1
        } else {
            let id = self.next_id;
            self.next_id += 1;
            self.tx.execute(
                "INSERT INTO task_origin(task_id, origin_uuid, origin_task_id) VALUES (?,?,?)",
                rusqlite::params![id, canon.0, canon.1],
            )?;
            id
        };
        self.tasks.insert(canon, local);
        Ok(local)
    }

    fn anomaly(&mut self, msg: String) {
        self.anomalies.push(msg);
    }

    /// Names the state event that put a task where replay has it.
    ///
    /// Half of every transition refusal: the reader needs the losing event and
    /// the winning one to work out which of the two machines is wrong, and a
    /// message naming only the event that failed sends them looking in the one
    /// store that is probably fine.
    fn previously(&self, task: i64) -> String {
        match self.last_state.get(&task) {
            Some((uuid, id, ts)) => format!("It was last moved by event {uuid}/{id} at {ts}."),
            None => "No earlier state event for it exists in the merged log.".to_string(),
        }
    }

    fn apply(&mut self, ev: &LedgerEvent) -> Result<()> {
        let payload: Value = ev
            .payload
            .as_deref()
            .and_then(|p| serde_json::from_str(p).ok())
            .unwrap_or(Value::Null);
        let Some(origin_task) = ev.task_id else {
            // Store-scoped events have no projection to write. None exist yet;
            // reporting rather than ignoring is what makes the first one visible.
            self.anomaly(format!(
                "event {}/{}: kind `{}` has a null task_id and no projection is defined for \
                 store-scoped events",
                ev.origin_uuid, ev.origin_id, ev.kind
            ));
            return Ok(());
        };
        let canon = self.canonical(&ev.origin_uuid, origin_task);

        if is_creation(&ev.kind, &payload) {
            return self.create(ev, &payload, canon);
        }
        let Some(task) = self.ensure_task(&canon)? else {
            self.anomaly(format!(
                "event {}/{}: kind `{}` names task {}/{}, which no creation event in the \
                 merged log ever minted",
                ev.origin_uuid, ev.origin_id, ev.kind, canon.0, canon.1
            ));
            self.unreconstructable.insert(canon);
            return Ok(());
        };

        match ev.kind.as_str() {
            "state_change" => self.state_change(ev, &payload, task)?,
            "edit" => self.edit(ev, &payload, task)?,
            "tag_added" => {
                if let Some(name) = payload.get("name").and_then(Value::as_str) {
                    self.said_tags.insert((task, name.to_string()));
                    self.tx.execute(
                        "INSERT OR IGNORE INTO tag(task_id, name) VALUES (?,?)",
                        rusqlite::params![task, name],
                    )?;
                }
            }
            "tag_removed" => {
                if let Some(name) = payload.get("name").and_then(Value::as_str) {
                    self.said_tags.insert((task, name.to_string()));
                    self.tx.execute(
                        "DELETE FROM tag WHERE task_id = ? AND name = ?",
                        rusqlite::params![task, name],
                    )?;
                }
            }
            "dep_added" => {
                let Some(on) = self.target(ev, &payload, "on") else {
                    return Ok(());
                };
                self.said_deps.insert((task, on));
                let mode = payload
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("blocks");
                self.tx.execute(
                    "INSERT INTO dep(task_id, depends_on_task_id, mode) VALUES (?1,?2,?3)
                       ON CONFLICT(task_id, depends_on_task_id) DO UPDATE SET mode = excluded.mode",
                    rusqlite::params![task, on, mode],
                )?;
            }
            "dep_removed" => {
                let Some(on) = self.target(ev, &payload, "on") else {
                    return Ok(());
                };
                self.said_deps.insert((task, on));
                self.tx.execute(
                    "DELETE FROM dep WHERE task_id = ? AND depends_on_task_id = ?",
                    rusqlite::params![task, on],
                )?;
            }
            "relation_add" => {
                let Some(to) = self.target(ev, &payload, "to") else {
                    return Ok(());
                };
                self.said_relations.insert((
                    task,
                    to,
                    payload
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ));
                self.tx.execute(
                    "INSERT OR IGNORE INTO relation(from_task_id, to_task_id, kind, created_at)
                     VALUES (?,?,?,?)",
                    rusqlite::params![
                        task,
                        to,
                        payload.get("kind").and_then(Value::as_str).unwrap_or(""),
                        ev.ts
                    ],
                )?;
            }
            "relation_removed" => {
                let Some(to) = self.target(ev, &payload, "to") else {
                    return Ok(());
                };
                self.said_relations.insert((
                    task,
                    to,
                    payload
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ));
                self.tx.execute(
                    "DELETE FROM relation WHERE from_task_id = ? AND to_task_id = ? AND kind = ?",
                    rusqlite::params![
                        task,
                        to,
                        payload.get("kind").and_then(Value::as_str).unwrap_or("")
                    ],
                )?;
            }
            // Recorded about a task, changing no row. A no-op by design.
            "note" | "decision" | "artifact" | "blocker" => {}
            // Any other kind is an annotation too, and the tally is what says
            // so out loud. This used to raise an anomaly, which was wrong twice
            // over: `qp log` takes the kind as a free-form argument, so the set
            // is open BY DESIGN and cannot be enumerated in advance — real
            // stores carry `finding`, `review`, `progress`, `handoff` and more
            // that nobody declared anywhere — and the event survives in full
            // regardless, written back into `event` by a restore and into
            // `foreign_event` by a merge. Calling that damage put a clean
            // ledger behind `--force` for writing its own log.
            //
            // It is still counted and named rather than passed over, because
            // the other thing an unrecognised kind could be is a structural
            // event from a NEWER quipu, and there the empty projection is a
            // real gap. Replay cannot tell the two apart, so it reports what it
            // saw and leaves the judgement to the reader.
            other => *self.opaque.entry(other.to_string()).or_insert(0) += 1,
        }
        Ok(())
    }

    /// Resolves a display id in a payload to a local rowid.
    ///
    /// The id is spelled in the ORIGIN store's prefix and numbered in its
    /// rowids, so it goes through the canonical map like everything else.
    fn target(&mut self, ev: &LedgerEvent, payload: &Value, key: &str) -> Option<i64> {
        let raw = payload.get(key).and_then(Value::as_str);
        // The origin store's prefix, not this one's — parsed for its rowid and
        // deliberately not checked against the local prefix.
        let Some(rowid) = raw.and_then(|s| id::parse(s).ok()).map(|r| r.rowid) else {
            self.anomaly(format!(
                "event {}/{}: kind `{}` has no readable `{key}` display id (found {raw:?})",
                ev.origin_uuid, ev.origin_id, ev.kind
            ));
            return None;
        };
        let canon = self.canonical(&ev.origin_uuid, rowid);
        match self.ensure_task(&canon).ok().flatten() {
            Some(id) => Some(id),
            None => {
                self.anomaly(format!(
                    "event {}/{}: kind `{}` points `{key}` at task {}/{}, which the merged log \
                     never created",
                    ev.origin_uuid, ev.origin_id, ev.kind, canon.0, canon.1
                ));
                self.unreconstructable.insert(canon);
                None
            }
        }
    }

    /// Births a task. The only event that brings one into existence.
    fn create(&mut self, ev: &LedgerEvent, payload: &Value, canon: (String, i64)) -> Result<()> {
        if self.hoisted.remove(&canon) {
            // Its own creation event, reached at last. Already applied.
            return Ok(());
        }
        if self.tasks.contains_key(&canon) {
            // A second creation for one canonical name would be two tickets
            // wearing one identity, which no ordering can repair.
            let existing = self.tasks[&canon];
            let live: i64 =
                self.tx
                    .query_row("SELECT COUNT(*) FROM task WHERE id = ?", [existing], |r| {
                        r.get(0)
                    })?;
            if live != 0 {
                self.anomaly(format!(
                    "event {}/{}: task {}/{} is created twice in the merged log",
                    ev.origin_uuid, ev.origin_id, canon.0, canon.1
                ));
                return Ok(());
            }
        }
        let task = self.mint(canon)?;
        let title = payload
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let tier = opt_str(payload.get("tier"));
        let description = opt_str(payload.get("description"));
        let state = payload
            .get("to")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        self.tx.execute(
            "INSERT INTO task(id, display_id, title, tier, description, state, created_at)
             VALUES (?,?,?,?,?,?,?)",
            rusqlite::params![
                task,
                id::encode(task, &self.prefix),
                title,
                tier,
                description,
                state,
                ev.ts
            ],
        )?;
        self.field_value.insert((task, "title".into()), Some(title));
        self.field_value.insert((task, "tier".into()), tier);
        self.field_value
            .insert((task, "description".into()), description);
        self.last_state
            .insert(task, (ev.origin_uuid.clone(), ev.origin_id, ev.ts.clone()));
        if !order::is_birth_state(state) {
            self.anomaly(format!(
                "event {}/{} at {}: {} is created directly into `{state}`, which is not a \
                 state a task can come into existence in. The creation event is malformed.",
                ev.origin_uuid,
                ev.origin_id,
                ev.ts,
                id::encode(task, &self.prefix)
            ));
        }
        Ok(())
    }

    /// A guarded transition, plus the assignment lifecycle that rides on it.
    fn state_change(&mut self, ev: &LedgerEvent, payload: &Value, task: i64) -> Result<()> {
        let to = payload.get("to").and_then(Value::as_str).unwrap_or("");
        // An event with no `from` predates the origin-state fix. The move it
        // records is not in doubt — `to` is right there — only the expectation
        // to check it against, so the origin is taken from where replay has the
        // task and the guard below becomes a formality. That is a WEAKER check,
        // not a guess about content, and it is counted and reported rather than
        // waved through: refusing instead would make every store written before
        // that fix permanently unrestorable, turning a partial loss into a
        // total one. Events that DO carry a `from` are still checked absolutely.
        let from = match payload.get("from").and_then(Value::as_str) {
            Some(f) => f.to_string(),
            None => {
                self.unguarded += 1;
                let Some(actual) = db::state_of(self.tx, task)? else {
                    self.anomaly(format!(
                        "event {}/{}: state_change with no `from` names {}, which replay has \
                         not created",
                        ev.origin_uuid,
                        ev.origin_id,
                        id::encode(task, &self.prefix)
                    ));
                    return Ok(());
                };
                actual
            }
        };
        let recorded = payload.get("from").and_then(Value::as_str).is_some();
        if recorded {
            self.moved.insert(task);
        }
        let from = from.as_str();
        // Legality, which continuity does not imply. An event recording a move
        // the machine cannot make applies cleanly whenever the projection
        // happens to be in the state it claims to leave — which is how an
        // illegal `pending` -> `assigned` hop crosses from a damaged ledger
        // into a healthy store without anything objecting.
        //
        // It is checked on the derived origin too, and that is the case it was
        // built for: an event with no recorded origin is exactly where the
        // illegal hops in pre-fidelity logs live, and there the derived origin
        // is the only origin there will ever be. The wording says which of the
        // two it is, because the accusation is different — one log claims an
        // impossible move, the other claims a move that cannot be reached from
        // where replay actually stands.
        if !order::is_legal_transition(from, to) {
            let previous = self.previously(task);
            let name = id::encode(task, &self.prefix);
            // A RECORDED origin makes this the log's own claim, and the claim is
            // impossible. Nothing recovers from that, in either mode.
            if recorded {
                self.anomaly(format!(
                    "event {}/{} at {}: {name} `{from}` -> `{to}` is not an edge of the \
                     state machine, so no ordering of the merged log can justify it. \
                     {previous} {}",
                    ev.origin_uuid,
                    ev.origin_id,
                    ev.ts,
                    order::SKEW_HINT
                ));
                return Ok(());
            }
            // A DERIVED origin is a different accusation. The event records
            // where the task WENT — that much is not in doubt — and says
            // nothing about where it came from, because the binary that wrote
            // it predates the log recording origins. Only the origin replay
            // supplied is unreachable.
            //
            // Merging that into live data is still refused: it would fold a
            // move nothing can justify into a store that currently holds good
            // state. Restoring it into an empty store applies it, because
            // discarding it is not the cautious choice — it leaves a finished
            // ticket sitting in `pending` in someone's recovered store, and
            // measured against a real ledger that is most of the tasks the log
            // cannot fully justify. The destination is evidence; the missing
            // origin is the gap, and the gap is reported either way.
            let verdict = match self.mode {
                Mode::Merge => "REFUSED: a merge will not fold that into live data",
                Mode::Restore => {
                    "APPLIED ANYWAY: this is a restore into an empty store, where the \
                     destination the event records is better evidence than the origin it \
                     never carried"
                }
            };
            self.anomaly(format!(
                "event {}/{} at {}: {name} records no origin state, and replay has it in \
                 `{from}`, from which `{to}` is not reachable in one move. {verdict}. \
                 {previous} {}",
                ev.origin_uuid,
                ev.origin_id,
                ev.ts,
                order::SKEW_HINT
            ));
            if self.mode == Mode::Merge {
                return Ok(());
            }
        }
        // The guard, exactly as the live command wrote it: one conditional
        // UPDATE, and `changes() == 1` is the only thing that decides.
        let n = self.tx.execute(
            "UPDATE task SET state = ?1 WHERE id = ?2 AND state = ?3",
            rusqlite::params![to, task, from],
        )?;
        if n != 1 {
            let actual: Option<String> = self
                .tx
                .query_row("SELECT state FROM task WHERE id = ?", [task], |r| r.get(0))
                .ok();
            let previous = self.previously(task);
            self.anomaly(format!(
                "event {}/{} at {}: {} `{from}` -> `{to}` cannot be applied; the merged log \
                 left it in {:?}. {previous} Some transition into `{from}` is missing, or two \
                 stores moved the same task at once. {}",
                ev.origin_uuid,
                ev.origin_id,
                ev.ts,
                id::encode(task, &self.prefix),
                actual,
                order::SKEW_HINT
            ));
            return Ok(());
        }
        self.last_state
            .insert(task, (ev.origin_uuid.clone(), ev.origin_id, ev.ts.clone()));

        match to {
            "assigned" => {
                let agent = payload
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| ev.agent_id.clone());
                match agent {
                    Some(agent) => {
                        // Defence in depth for the one error the schema turns
                        // into an abort. `idx_assign_one_open` makes a second
                        // open assignment a raw `UNIQUE constraint failed` from
                        // inside rusqlite, which is not an anomaly — it is the
                        // end of the restore, with nothing written and nothing
                        // named. Anything the arm above did not manage to close
                        // is closed here and reported instead.
                        if let Some(open) = self.open_assignment(task)? {
                            self.anomaly(format!(
                                "event {}/{} at {}: {} moves to `assigned` from `{from}` while \
                                 {open}'s claim is still open; the release that ended it is not \
                                 in the log. Replay closed it with no recorded outcome so the \
                                 rest of the ledger could be rebuilt.",
                                ev.origin_uuid,
                                ev.origin_id,
                                ev.ts,
                                id::encode(task, &self.prefix),
                            ));
                            self.close_assignment(task, &ev.ts, None)?;
                        }
                        self.tx.execute(
                            "INSERT INTO assignment(task_id, agent_id, assigned_at) VALUES (?,?,?)",
                            rusqlite::params![task, agent, ev.ts],
                        )?;
                    }
                    None => self.anomaly(format!(
                        "event {}/{}: moved to `assigned` with no agent recorded, so replay \
                         cannot open the assignment",
                        ev.origin_uuid, ev.origin_id
                    )),
                }
            }
            "running" if from == "assigned" => {
                self.tx.execute(
                    "UPDATE assignment SET claimed_at = ?
                      WHERE task_id = ? AND completed_at IS NULL",
                    rusqlite::params![ev.ts, task],
                )?;
            }
            // Leaving a held state releases the claim — EVERY destination, not
            // an enumerated three. `assigned` -> `running` is the sole edge
            // that stays inside the hold and it is the arm above; `ready` was
            // the destination this list was missing, and the missing arm cost
            // two real stores their entire ledger. Keying on the origin rather
            // than on the destination is what stops the next unlisted hop from
            // doing it again.
            _ if order::holds_assignment(from) && to != "running" => {
                let outcome = self.release_outcome(ev, payload, task, from, to);
                self.close_assignment(task, &ev.ts, outcome)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Which assignment outcome a move out of a held state records.
    ///
    /// `None` is not a failure to look: it is the recorded fact that the log
    /// did not say. Guessing an outcome would put a value in the column that no
    /// event supports, and refusing to close would leave the claim open — the
    /// shape that took two real stores down. So it closes, unclassified, and
    /// the anomaly names the task and the event a reader has to go and look at.
    fn release_outcome(
        &mut self,
        ev: &LedgerEvent,
        payload: &Value,
        task: i64,
        from: &str,
        to: &str,
    ) -> Option<&'static str> {
        match to {
            "done" => return Some("success"),
            "cancelled" => return Some("cancelled"),
            _ => {}
        }
        match payload.get("via").and_then(Value::as_str) {
            Some("abandon") | Some("block") => Some("abandoned"),
            Some("reclaim") => Some("reclaimed"),
            other => {
                self.anomaly(format!(
                    "event {}/{} at {}: {} was released from `{from}` to `{to}` with \
                     via={other:?}, which replay cannot map to an assignment outcome. The \
                     claim is closed with none recorded rather than left open.",
                    ev.origin_uuid,
                    ev.origin_id,
                    ev.ts,
                    id::encode(task, &self.prefix)
                ));
                None
            }
        }
    }

    /// The agent holding this task's open claim, if one is open.
    fn open_assignment(&self, task: i64) -> Result<Option<String>> {
        Ok(self
            .tx
            .query_row(
                "SELECT agent_id FROM assignment WHERE task_id = ? AND completed_at IS NULL",
                [task],
                |r| r.get(0),
            )
            .ok())
    }

    fn close_assignment(&self, task: i64, ts: &str, outcome: Option<&str>) -> Result<()> {
        self.tx.execute(
            "UPDATE assignment SET completed_at = ?1, outcome = ?2
              WHERE task_id = ?3 AND completed_at IS NULL",
            rusqlite::params![ts, outcome, task],
        )?;
        Ok(())
    }

    /// A field-level compare-and-swap, which is what an `edit` event records.
    ///
    /// Three cases, and only the third is a conflict: different fields both
    /// apply, the same field edited in sequence chains through its `from`
    /// values, and the same field edited from the SAME `from` twice is two
    /// stores branching off one prior state.
    fn edit(&mut self, ev: &LedgerEvent, payload: &Value, task: i64) -> Result<()> {
        let Some(changes) = payload.get("changes").and_then(Value::as_object) else {
            self.anomaly(format!(
                "event {}/{}: edit with no `changes` object",
                ev.origin_uuid, ev.origin_id
            ));
            return Ok(());
        };
        for (field, delta) in changes {
            if !EDITABLE.contains(&field.as_str()) {
                self.anomaly(format!(
                    "event {}/{}: edit names field `{field}`, which replay does not project",
                    ev.origin_uuid, ev.origin_id
                ));
                continue;
            }
            let key = (task, field.clone());
            self.said_fields.insert(key.clone());
            let from = opt_str(delta.get("from"));
            let to = opt_str(delta.get("to"));
            let head = Branch {
                from: from.clone(),
                value: to.clone(),
                origin_uuid: ev.origin_uuid.clone(),
                origin_id: ev.origin_id,
                ts: ev.ts.clone(),
            };

            // Case 3, already open: the field is held at the value both sides
            // agreed on, and later edits extend whichever branch they chain
            // onto rather than silently picking one.
            if let Some(branches) = self.branches.get_mut(&key) {
                match branches.iter_mut().find(|b| b.value == from) {
                    Some(b) => *b = head,
                    None => branches.push(head),
                }
                continue;
            }

            let cur = self.field_value.get(&key).cloned().unwrap_or(None);
            if from == cur {
                // Cases 1 and 2: the compare succeeded, so swap.
                self.write_field(task, field, &to)?;
                self.field_value.insert(key.clone(), to);
                self.last_edit.insert(key, head);
                continue;
            }

            // A creation event written before the log carried `tier` and
            // `description` left those fields NULL, so the first edit of one
            // chains from a value replay never saw. The event itself records
            // what it was, so this recovers a value from the log rather than
            // guessing at one — and only where no edit has touched the field,
            // which is exactly the shape that gap makes. A genuine concurrent
            // edit still lands in the arm below, because the second of the two
            // finds a prior edit carrying the same `from`.
            if cur.is_none() && from.is_some() && !self.last_edit.contains_key(&key) {
                self.repaired += 1;
                self.write_field(task, field, &to)?;
                self.field_value.insert(key.clone(), to);
                self.last_edit.insert(key, head);
                continue;
            }

            let prior = self.last_edit.get(&key).cloned();
            match prior {
                // Case 3: both edits claim the same prior value, which is the
                // definition of concurrency. Record both, apply neither.
                Some(prior) if prior.from == from => {
                    self.write_field(task, field, &from)?;
                    self.field_value.insert(key.clone(), from.clone());
                    self.conflict_base.insert(key.clone(), from);
                    self.branches.insert(key, vec![prior, head]);
                }
                _ => self.anomaly(format!(
                    "event {}/{}: edit of `{field}` on {} expects {:?} but the merged log left \
                     it at {:?}; the chain of `from` values is broken",
                    ev.origin_uuid,
                    ev.origin_id,
                    id::encode(task, &self.prefix),
                    from,
                    cur
                )),
            }
        }
        Ok(())
    }

    fn write_field(&self, task: i64, field: &str, value: &Option<String>) -> Result<()> {
        // `field` is whitelisted against EDITABLE above, which is the only
        // reason it can be interpolated here.
        self.tx.execute(
            &format!("UPDATE task SET {field} = ?1 WHERE id = ?2"),
            rusqlite::params![value, task],
        )?;
        Ok(())
    }

    /// Writes out what the pass detected and reports what it could not apply.
    fn finish(mut self) -> Result<Summary> {
        let mut conflicts = 0usize;
        let mut conflicted: Vec<i64> = Vec::new();
        let branches = std::mem::take(&mut self.branches);
        for ((task, field), heads) in &branches {
            let base = self
                .conflict_base
                .get(&(*task, field.clone()))
                .cloned()
                .unwrap_or(None);
            for head in heads {
                self.tx.execute(
                    "INSERT OR REPLACE INTO conflict(task_id, field, origin_uuid, origin_event_id,
                                                     base, value, ts)
                     VALUES (?,?,?,?,?,?,?)",
                    rusqlite::params![
                        task,
                        field,
                        head.origin_uuid,
                        head.origin_id,
                        base,
                        head.value,
                        head.ts
                    ],
                )?;
                conflicts += 1;
            }
            conflicted.push(*task);
        }
        conflicted.sort_unstable();
        conflicted.dedup();
        for task in &conflicted {
            self.tx.execute(
                "INSERT OR IGNORE INTO tag(task_id, name) VALUES (?,?)",
                rusqlite::params![task, CONFLICT_TAG],
            )?;
        }
        let tasks: i64 = self
            .tx
            .query_row("SELECT COUNT(*) FROM task", [], |r| r.get(0))?;
        Ok(Summary {
            tasks: tasks as usize,
            conflicts,
            conflicted_tasks: conflicted
                .iter()
                .map(|t| id::encode(*t, &self.prefix))
                .collect(),
            anomalies: self.anomalies,
            opaque: self.opaque,
            unguarded: self.unguarded,
            said_tags: self.said_tags,
            said_deps: self.said_deps,
            said_relations: self.said_relations,
            said_fields: self.said_fields,
            moved: self.moved,
            repaired: self.repaired,
            unreconstructable: self.unreconstructable,
        })
    }
}

/// Whether a `state_change` is the event that brought its task into existence.
///
/// `from: null` says so outright. An event with NO `from` at all predates the
/// origin-state fix, and there the `title` is the tell: only a creation carries
/// one, because the INSERT that sets it has no `edit` event to say so. Without
/// this second arm every task in every store written before that fix looks like
/// a task the log never created — very nearly all of them in a real store
/// measured against — and the whole ledger reads as unrestorable.
fn is_creation(kind: &str, payload: &Value) -> bool {
    if kind != "state_change" {
        return false;
    }
    match payload.get("from") {
        Some(v) => v.is_null(),
        None => payload.get("title").and_then(Value::as_str).is_some(),
    }
}

/// A JSON value as an optional string, mapping `null` and absence alike to `None`.
fn opt_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_string)
}
