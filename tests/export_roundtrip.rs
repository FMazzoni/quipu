//! Export round-trip: a store must survive a trip out to the ledger and back.
//!
//! `replay_equivalence` proves one half of the claim — that the event log can
//! rebuild the projection. This proves the other half: that `qp export` puts
//! the whole of that log onto disk, in a form a reader can pick up cold and
//! turn back into the same store. Neither half is a backup on its own. A log
//! that rebuilds the projection but cannot be written out is unreachable, and a
//! file that is written out but incomplete restores something that merely
//! resembles the original.
//!
//! # The bar
//!
//! Export a mutated store, read the files into an **empty** store that shares
//! nothing with it — not even its `project_uuid` or its display prefix — and
//! assert the projections are identical, column by column, across `task`,
//! `dep`, `tag`, `assignment` and `relation`. The diff is the shared one from
//! `common`, so this test and the replay property hold the same line and cannot
//! drift apart into two different definitions of "identical".
//!
//! # The reader is `qp import --restore`
//!
//! It used to live in this file, because reading a ledger back was not a
//! command. Now that it is, the test drives the binary instead of carrying a
//! second reader beside it: two readers drift, and the copy that drifts is the
//! one whose property then proves nothing — the same argument that put the
//! replayer in `common` rather than in two test files.
//!
//! What the test still owns is the *bar*. It exports a store, restores into one
//! that shares nothing with it — not its `project_uuid`, not its display prefix
//! — and holds the result to `common`'s projection diff. The export has to be
//! complete enough to rebuild from with no access to the source store, which is
//! the property a backup format actually needs, and the file layout is still
//! pinned by assertions that fail when it changes.
//!
//! # What the round trip does not carry
//!
//! `schema_version` and the default-tag set are store state that no event
//! records, so they travel in the batch header as their value at export time.
//! A restore therefore recovers what they *are*, not how they got there. That
//! is a known gap in the log, not in the export, and it is asserted here rather
//! than assumed so it fails loudly if either becomes an event.

mod common;

use common::*;
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Deliberately different from the source store's, so nothing can pass by
/// accident of both sides having been initialised the same way.
const FOREIGN_PREFIX: &str = "ZZZ";

// ---------------------------------------------------------------------------
// the property
// ---------------------------------------------------------------------------

#[test]
fn a_store_round_trips_through_the_exported_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.sqlite");
    let dst = tmp.path().join("restored.sqlite");
    let ledger = tmp.path().join("ledger");

    init_store(&src);
    run_mutation_script(&src);
    export(&src, &ledger);

    init_foreign_store(&dst);
    assert_empty_projection(&dst);

    restore(&dst, &ledger.join(project_uuid(&src)));
    let report = diff_projections(&src, &dst);

    assert!(
        report.is_empty(),
        "the exported ledger does not restore the store.\n\
         Either the export dropped something the log holds, or it wrote it in a \
         form a reader cannot apply.\n\n{report}"
    );
}

/// The log itself must survive intact, not merely produce the same projection.
///
/// A projection can match while the history behind it is lossy — the same end
/// state reached by fewer steps still diffs clean. The ledger is the record of
/// decisions, so the events are the payload, not a means to one.
#[test]
fn every_event_survives_byte_for_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.sqlite");
    let dst = tmp.path().join("restored.sqlite");
    let ledger = tmp.path().join("ledger");

    init_store(&src);
    run_mutation_script(&src);
    export(&src, &ledger);
    init_foreign_store(&dst);
    restore(&dst, &ledger.join(project_uuid(&src)));

    const SQL: &str = "SELECT id || '|' || COALESCE(task_id,'') || '|' || ts || '|' ||
                              kind || '|' || COALESCE(agent_id,'') || '|' ||
                              COALESCE(payload,'')
                         FROM event ORDER BY id";
    let want = rows(&Connection::open(&src).unwrap(), SQL);
    let got = rows(&Connection::open(&dst).unwrap(), SQL);
    assert!(!want.is_empty(), "the mutation script logged nothing");
    assert_eq!(
        want, got,
        "the restored event log is not the one that was exported"
    );
}

/// Store identity travels in the envelope, which is the whole of QP-225.
///
/// The restored store is initialised with a *different* uuid and a different
/// prefix, so both can only end up matching the source if the batch header
/// carried them. That is also what a cross-store aggregator reads to namespace
/// display ids, and what the replay property refuses to take from the log.
#[test]
fn the_envelope_carries_identity_and_the_schema_version() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.sqlite");
    let dst = tmp.path().join("restored.sqlite");
    let ledger = tmp.path().join("ledger");

    init_store(&src);
    run_mutation_script(&src);
    export(&src, &ledger);
    init_foreign_store(&dst);

    let uuid = project_uuid(&src);
    let batches = read_ledger(&ledger, &uuid);
    let header = &batches[0].header;
    assert_eq!(header["format"], "quipu-export");
    assert_eq!(
        header["format_version"], 1,
        "every batch must say which format version wrote it, or a later format \
         change makes this one unreadable or silently misread"
    );
    assert_eq!(header["project_uuid"], uuid);
    assert_eq!(header["display_prefix"], PREFIX);
    assert_eq!(header["default_tags"][0], DEFAULT_TAG);

    // Before the import the two stores agree on no part of their identity;
    // after it they agree on everything the envelope is responsible for.
    // `schema_version` is excluded from the "before" half on purpose: both
    // stores were minted by the same binary, so it matches trivially and would
    // be a vacuous assertion rather than a weak one.
    let s = Connection::open(&src).unwrap();
    let d = Connection::open(&dst).unwrap();
    for key in ["project_uuid", "display_prefix"] {
        assert_ne!(
            meta(&s, key),
            meta(&d, key),
            "`{key}` was equal before import"
        );
    }
    assert_eq!(header["schema_version"], meta(&s, "schema_version"));

    restore(&dst, &ledger.join(&uuid));
    for key in ["project_uuid", "display_prefix", "schema_version"] {
        assert_eq!(
            meta(&s, key),
            meta(&d, key),
            "`{key}` did not survive export"
        );
    }
    assert_eq!(
        rows(&s, "SELECT name FROM default_tag ORDER BY name"),
        rows(&d, "SELECT name FROM default_tag ORDER BY name")
    );
}

// ---------------------------------------------------------------------------
// the file layout
// ---------------------------------------------------------------------------

/// Batches are append-only and immutable: a second export adds a file and
/// rewrites nothing.
///
/// This is the property that makes a ledger cheap to keep in version control
/// and safe to merge. One file rewritten on every export would defeat both — a
/// changed blob deltas against nothing, and two machines rewriting the same
/// path is a conflict rather than a union.
#[test]
fn a_second_export_appends_a_new_batch_and_rewrites_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.sqlite");
    let ledger = tmp.path().join("ledger");
    let uuid_dir = |u: &str| ledger.join(u);

    init_store(&src);
    ok(&src, &["add", "first"]);
    export(&src, &ledger);
    let uuid = project_uuid(&src);
    let first = batch_files(&uuid_dir(&uuid));
    assert_eq!(first.len(), 1, "one export, one batch: {first:?}");
    let frozen = std::fs::read(&first[0]).unwrap();

    // Nothing new to say: no file, and no touching the existing one.
    export(&src, &ledger);
    assert_eq!(
        batch_files(&uuid_dir(&uuid)),
        first,
        "an empty export wrote a file"
    );

    ok(&src, &["add", "second"]);
    export(&src, &ledger);
    let second = batch_files(&uuid_dir(&uuid));
    assert_eq!(
        second.len(),
        2,
        "the second export did not add a batch: {second:?}"
    );
    assert_eq!(
        std::fs::read(&first[0]).unwrap(),
        frozen,
        "an existing batch was rewritten; batches are immutable"
    );

    // Ranges are contiguous and disjoint, so the union of the two files is the
    // whole log exactly once.
    let ranges: Vec<(i64, i64)> = second.iter().map(|p| range_of(p, &uuid)).collect();
    assert!(
        ranges[0].1 < ranges[1].0,
        "batch ranges overlap: {ranges:?}"
    );

    let events = read_ledger(&ledger, &uuid)
        .iter()
        .flat_map(|b| b.events.iter().map(|e| e.id))
        .collect::<Vec<_>>();
    let expected: Vec<i64> = read_events(&src).iter().map(|e| e.id).collect();
    assert_eq!(events, expected, "the batches do not tile the log");
}

/// Two stores exporting into one root cannot collide, so merging two ledgers is
/// a set union of files with nothing to reconcile.
#[test]
fn two_stores_share_a_ledger_root_without_colliding() {
    let tmp = tempfile::tempdir().unwrap();
    let ledger = tmp.path().join("ledger");
    let a = tmp.path().join("a.sqlite");
    let b = tmp.path().join("b.sqlite");

    for db in [&a, &b] {
        init_store(db);
        ok(db, &["add", "same title in both stores"]);
        export(db, &ledger);
    }

    let (ua, ub) = (project_uuid(&a), project_uuid(&b));
    assert_ne!(ua, ub);

    // One directory per project, so a later split is a directory move.
    let dirs: Vec<String> = std::fs::read_dir(&ledger)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        dirs.len(),
        2,
        "expected one directory per project: {dirs:?}"
    );

    // And the filenames themselves are disjoint even flattened into one bag,
    // which is what makes the union safe regardless of layout.
    let names = |u: &str| -> Vec<String> {
        batch_files(&ledger.join(u))
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    let (na, nb) = (names(&ua), names(&ub));
    assert!(!na.is_empty() && !nb.is_empty());
    for n in &na {
        assert!(
            !nb.contains(n),
            "two stores minted the same batch name: {n}"
        );
    }
}

/// Negative control: prove the round trip can fail, and fail where it should.
///
/// Without this, a reader that silently restored nothing and a diff that
/// compared nothing would both report success.
///
/// The loss is a whole batch file, which is the shape the failure actually
/// takes: batches are immutable and written through a rename, so a *partial*
/// one cannot exist under a trusted name, but a missing one can — the exporter
/// derives its high-water mark from the filenames precisely so that deleting a
/// batch is a recoverable state rather than a corrupt one.
#[test]
fn a_ledger_missing_its_last_batch_is_reported_as_a_difference() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.sqlite");
    let dst = tmp.path().join("restored.sqlite");
    let ledger = tmp.path().join("ledger");

    init_store(&src);
    run_mutation_script(&src);
    export(&src, &ledger);
    // A second batch, so the ledger has a tail to lose.
    ok(&src, &["add", "written after the first batch"]);
    export(&src, &ledger);

    let dir = ledger.join(project_uuid(&src));
    let batches = batch_files(&dir);
    assert_eq!(batches.len(), 2, "expected two batches: {batches:?}");
    std::fs::remove_file(&batches[1]).unwrap();

    init_foreign_store(&dst);
    restore(&dst, &dir);
    let diffs = diff_projections(&src, &dst);
    assert!(
        diffs.contains("projection `task`") && diffs.contains("missing from replay"),
        "a ledger missing a batch must be reported as missing rows. got:\n{diffs}"
    );
}

// ---------------------------------------------------------------------------
// the reader
// ---------------------------------------------------------------------------

/// One batch file, parsed: its header and the events it carries.
pub struct Batch {
    pub header: serde_json::Value,
    pub events: Vec<Ev>,
}

/// Reads a project's whole ledger directory, in batch-name order.
///
/// Deduplicates by event id: overlapping batches are legal (re-exporting into a
/// directory that lost a file produces them) and an event applied twice is not.
fn read_ledger(root: &Path, project_uuid: &str) -> Vec<Batch> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for path in batch_files(&root.join(project_uuid)) {
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines = text.lines();
        let header: serde_json::Value =
            serde_json::from_str(lines.next().expect("empty batch file")).unwrap();
        assert_eq!(
            header["record"],
            "header",
            "a batch's first line must be its header, so the file is \
             interpretable on its own: {}",
            path.display()
        );
        assert_eq!(header["project_uuid"], project_uuid);

        let mut events = Vec::new();
        for line in lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["record"], "event");
            let id = v["id"].as_i64().unwrap();
            if !seen.insert(id) {
                continue;
            }
            events.push(Ev {
                id,
                task_id: v["task_id"].as_i64(),
                ts: v["ts"].as_str().unwrap().to_string(),
                kind: v["kind"].as_str().unwrap().to_string(),
                agent_id: v["agent_id"].as_str().map(str::to_string),
                payload: v.get("payload").cloned().unwrap_or(serde_json::Value::Null),
            });
        }
        let lo = header["first_event_id"].as_i64().unwrap();
        let hi = header["last_event_id"].as_i64().unwrap();
        assert_eq!(
            (lo, hi),
            range_of(&path, project_uuid),
            "the header's range and the filename's range disagree: {}",
            path.display()
        );
        out.push(Batch { header, events });
    }
    out
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn export(db: &Path, out: &Path) {
    ok(db, &["export", "--out", out.to_str().unwrap()]);
}

/// Rebuilds `db` from one store's batch directory, through the real command.
///
/// `--restore` is spelled out because the mode is never inferred: it adopts the
/// ledger's identity, which is the opposite of what `--merge` does to a store.
fn restore(db: &Path, batch_dir: &Path) {
    ok(
        db,
        &["import", "--from", batch_dir.to_str().unwrap(), "--restore"],
    );
}

/// A destination store that shares nothing with the source it will restore.
fn init_foreign_store(db: &Path) {
    ok(db, &["init", "--prefix", FOREIGN_PREFIX]);
}

fn batch_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    v.sort();
    v
}

/// The event range a batch's *filename* claims, which is what the exporter's
/// high-water scan trusts.
fn range_of(path: &Path, project_uuid: &str) -> (i64, i64) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let rest = name
        .strip_prefix(project_uuid)
        .and_then(|r| r.strip_prefix('-'))
        .and_then(|r| r.strip_suffix(".jsonl"))
        .unwrap_or_else(|| panic!("batch `{name}` is not named after its origin store"));
    let (lo, hi) = rest.split_once('-').unwrap();
    (lo.parse().unwrap(), hi.parse().unwrap())
}

fn project_uuid(db: &Path) -> String {
    meta(&Connection::open(db).unwrap(), "project_uuid")
}

fn meta(conn: &Connection, key: &str) -> String {
    conn.query_row("SELECT value FROM meta WHERE key = ?", [key], |r| r.get(0))
        .unwrap()
}
