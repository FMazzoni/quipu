//! Merge semantics: importing must be idempotent, convergent and loud.
//!
//! `export_roundtrip` proves the easy direction — a ledger restores into an
//! empty store. This file covers the direction where the decisions are, folding
//! batches into a store that already holds work of its own.
//!
//! # Convergence is the property, and it is not about display ids
//!
//! Two machines that have seen the same batches must hold the same tickets,
//! with the same fields, edges, history and conflicts. They will not agree on
//! *rowids*: each mints a local number for a ticket it did not create, above
//! whatever it had already used, so the same ticket can be `AAA-4` here and
//! `BBB-1` there. That is naming, not content — a ticket's identity is the uuid
//! of the store that created it plus the rowid it holds there, which is exactly
//! what `task_origin` and the batch header's copy of it carry.
//!
//! So the comparison is over *canonical* names. Comparing local ids instead
//! would fail on two stores that are in perfect agreement, and passing it would
//! require renumbering tickets on every sync — which is the one thing a store
//! must never do, because the numbers are already written down elsewhere.
//!
//! # Why arrival order gets its own test
//!
//! A union of batches is a set; replay needs a sequence. Ordering by
//! `(ts, project_uuid, event id)` makes that sequence a function of the set
//! rather than of the delivery schedule, and the only way to test it is to
//! deliver the same batches in different orders and demand the same answer.
//! Two peers is not enough to make "order" mean anything, so there are three.

mod common;

use common::{diff_projections, ok, qp};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// A store's whole projection, rendered under canonical task names.
///
/// Deterministic text, so an assertion failure prints the disagreement rather
/// than a boolean. See the module header for why the canonical name and not the
/// rowid is the thing compared.
fn canonical_dump(db: &Path) -> String {
    let conn = Connection::open(db).unwrap();
    let local: String = conn
        .query_row("SELECT value FROM meta WHERE key='project_uuid'", [], |r| {
            r.get(0)
        })
        .unwrap();

    // `task_origin` names only what this store did not mint; everything else is
    // native, and its canonical name follows from its own rowid.
    let mut origin = std::collections::HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT task_id, origin_uuid, origin_task_id FROM task_origin")
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    format!("{}/{}", r.get::<_, String>(1)?, r.get::<_, i64>(2)?),
                ))
            })
            .unwrap();
        for row in rows {
            let (id, canon) = row.unwrap();
            origin.insert(id, canon);
        }
    }
    let name = |id: i64| -> String {
        origin
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("{local}/{id}"))
    };

    let mut lines: Vec<String> = Vec::new();
    let mut push = |section: &str, sql: &str, render: &dyn Fn(&rusqlite::Row) -> String| {
        let mut stmt = conn.prepare(sql).unwrap();
        let mut rows: Vec<String> = stmt
            .query_map([], |r| Ok(render(r)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows.sort();
        for r in rows {
            lines.push(format!("{section} {r}"));
        }
    };

    push(
        "task",
        "SELECT id, title, tier, description, state FROM task",
        &|r| {
            format!(
                "{} | {:?} | {:?} | {:?} | {}",
                name(r.get::<_, i64>(0).unwrap()),
                r.get::<_, String>(1).unwrap(),
                r.get::<_, Option<String>>(2).unwrap(),
                r.get::<_, Option<String>>(3).unwrap(),
                r.get::<_, String>(4).unwrap(),
            )
        },
    );
    push("tag", "SELECT task_id, name FROM tag", &|r| {
        format!(
            "{} | {}",
            name(r.get::<_, i64>(0).unwrap()),
            r.get::<_, String>(1).unwrap()
        )
    });
    push(
        "dep",
        "SELECT task_id, depends_on_task_id, mode FROM dep",
        &|r| {
            format!(
                "{} -> {} | {}",
                name(r.get::<_, i64>(0).unwrap()),
                name(r.get::<_, i64>(1).unwrap()),
                r.get::<_, String>(2).unwrap()
            )
        },
    );
    push(
        "relation",
        "SELECT from_task_id, to_task_id, kind FROM relation",
        &|r| {
            format!(
                "{} -> {} | {}",
                name(r.get::<_, i64>(0).unwrap()),
                name(r.get::<_, i64>(1).unwrap()),
                r.get::<_, String>(2).unwrap()
            )
        },
    );
    push(
        "assignment",
        "SELECT task_id, agent_id, claimed_at IS NOT NULL, completed_at IS NOT NULL,
                COALESCE(outcome,'-') FROM assignment",
        &|r| {
            format!(
                "{} | {} | claimed={} | closed={} | {}",
                name(r.get::<_, i64>(0).unwrap()),
                r.get::<_, String>(1).unwrap(),
                r.get::<_, i64>(2).unwrap(),
                r.get::<_, i64>(3).unwrap(),
                r.get::<_, String>(4).unwrap(),
            )
        },
    );
    push(
        "conflict",
        "SELECT task_id, field, COALESCE(base,'<null>'), COALESCE(value,'<null>'),
                origin_uuid, origin_event_id FROM conflict",
        &|r| {
            format!(
                "{} | {} | base={:?} | value={:?} | from {}#{}",
                name(r.get::<_, i64>(0).unwrap()),
                r.get::<_, String>(1).unwrap(),
                r.get::<_, String>(2).unwrap(),
                r.get::<_, String>(3).unwrap(),
                r.get::<_, String>(4).unwrap(),
                r.get::<_, i64>(5).unwrap(),
            )
        },
    );
    lines.join("\n")
}

fn init(db: &Path, prefix: &str) {
    ok(db, &["init", "--prefix", prefix]);
}

fn export(db: &Path, ledger: &Path) {
    ok(db, &["export", "--out", ledger.to_str().unwrap()]);
}

fn merge(db: &Path, dir: &Path) {
    ok(db, &["import", "--from", dir.to_str().unwrap(), "--merge"]);
}

fn uuid(db: &Path) -> String {
    Connection::open(db)
        .unwrap()
        .query_row("SELECT value FROM meta WHERE key='project_uuid'", [], |r| {
            r.get(0)
        })
        .unwrap()
}

/// One store's batch directory inside a ledger root.
fn dir_of(ledger: &Path, db: &Path) -> PathBuf {
    ledger.join(uuid(db))
}

fn count(db: &Path, table: &str) -> i64 {
    Connection::open(db)
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

/// The event log's own clock has millisecond resolution, and several of these
/// tests depend on one write landing after another rather than beside it.
fn tick() {
    std::thread::sleep(std::time::Duration::from_millis(3));
}

// ---------------------------------------------------------------------------
// idempotence
// ---------------------------------------------------------------------------

/// Importing the same batch twice must change nothing the second time.
///
/// This is the property `qp sync` will lean on hardest: it re-reads a ledger
/// every time it runs, and most runs have nothing new in them. A reader that
/// duplicated on the second pass would grow the store without bound while
/// looking like it was working.
#[test]
fn importing_the_same_ledger_twice_duplicates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a.sqlite"), tmp.path().join("b.sqlite"));
    let ledger = tmp.path().join("ledger");

    init(&a, "AAA");
    init(&b, "BBB");
    ok(&a, &["add", "from a", "--tag", "core"]);
    ok(&a, &["add", "second from a"]);
    ok(&a, &["depends", "AAA-2", "--on", "AAA-1"]);
    ok(&b, &["add", "from b"]);
    export(&a, &ledger);

    merge(&b, &dir_of(&ledger, &a));
    let after_first = canonical_dump(&b);
    let counts: Vec<i64> = ["task", "tag", "dep", "foreign_event", "task_origin"]
        .iter()
        .map(|t| count(&b, t))
        .collect();

    merge(&b, &dir_of(&ledger, &a));
    assert_eq!(
        canonical_dump(&b),
        after_first,
        "a second import of the same ledger changed the projection"
    );
    let again: Vec<i64> = ["task", "tag", "dep", "foreign_event", "task_origin"]
        .iter()
        .map(|t| count(&b, t))
        .collect();
    assert_eq!(counts, again, "a second import duplicated rows");

    // And a third pass through a ledger root rather than a single batch
    // directory — the same batches, reached a different way.
    merge(&b, &ledger);
    assert_eq!(canonical_dump(&b), after_first);
}

/// A merge must not renumber a ticket the store already minted.
///
/// Native display ids are written down in commit messages, skills and other
/// people's notes. Sync earning the right to change them would make every one
/// of those references a silent lie.
#[test]
fn a_merge_leaves_native_display_ids_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a.sqlite"), tmp.path().join("b.sqlite"));
    let ledger = tmp.path().join("ledger");

    init(&a, "AAA");
    init(&b, "BBB");
    ok(&b, &["add", "native one"]);
    ok(&b, &["add", "native two"]);
    ok(&a, &["add", "foreign one"]);
    export(&a, &ledger);

    let before = Connection::open(&b).unwrap();
    let ids: Vec<String> = common::rows(&before, "SELECT display_id FROM task ORDER BY id");
    drop(before);

    merge(&b, &dir_of(&ledger, &a));
    let after = Connection::open(&b).unwrap();
    let now: Vec<String> = common::rows(&after, "SELECT display_id FROM task ORDER BY id");
    assert_eq!(now[..2], ids[..], "a merge renumbered a native task");
    assert_eq!(now.len(), 3, "the foreign task did not arrive: {now:?}");
    assert_eq!(now[2], "BBB-3", "the foreign task took a colliding id");
}

// ---------------------------------------------------------------------------
// convergence
// ---------------------------------------------------------------------------

/// Three stores, six merges, two delivery orders, one projection.
///
/// The point is the *order independence*: `B` sees C's batches then A's, `C`
/// sees A's then B's, and `A` gets both at once. If replay followed arrival
/// order instead of `(ts, project_uuid, event id)`, the three would settle into
/// three different stores and nothing would say so.
#[test]
fn peers_converge_whatever_order_the_batches_arrive_in() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.sqlite");
    let b = tmp.path().join("b.sqlite");
    let c = tmp.path().join("c.sqlite");
    let ledger = tmp.path().join("ledger");

    init(&a, "AAA");
    init(&b, "BBB");
    init(&c, "CCC");

    // A common ancestor: one ticket everyone has seen.
    ok(&a, &["add", "shared work", "--tag", "core"]);
    ok(&a, &["add", "a keeps this one"]);
    export(&a, &ledger);
    merge(&b, &dir_of(&ledger, &a));
    merge(&c, &dir_of(&ledger, &a));
    tick();

    // Now they diverge, offline from each other. B and C edit DIFFERENT fields
    // of the shared ticket, which is case 1 — both apply, no conflict.
    ok(&b, &["add", "b's own work"]);
    ok(&b, &["edit", "BBB-1", "--title", "shared work (b renamed)"]);
    ok(&b, &["tag", "BBB-1", "add", "prio:high"]);
    tick();
    ok(&c, &["add", "c's own work"]);
    ok(
        &c,
        &["edit", "CCC-1", "--description", "c filled the body in"],
    );
    ok(&c, &["relation", "add", "CCC-2", "related-to", "CCC-1"]);
    tick();

    // A works on its own ticket, and moves it through the state machine so the
    // merge has assignments and transitions to reconcile, not just fields.
    ok(&a, &["assign", "AAA-2", "--to", "agentA"]);
    ok(&a, &["claim", "AAA-2", "--as", "agentA"]);
    ok(&a, &["complete", "AAA-2", "--as", "agentA"]);

    for db in [&a, &b, &c] {
        export(db, &ledger);
    }

    // Three different delivery schedules for the same set of batches.
    merge(&a, &ledger);
    merge(&b, &dir_of(&ledger, &c));
    merge(&b, &dir_of(&ledger, &a));
    merge(&c, &dir_of(&ledger, &a));
    merge(&c, &dir_of(&ledger, &b));

    let (da, db_, dc) = (canonical_dump(&a), canonical_dump(&b), canonical_dump(&c));
    assert_eq!(
        da, db_,
        "A and B diverged:\n--- A ---\n{da}\n--- B ---\n{db_}"
    );
    assert_eq!(
        db_, dc,
        "B and C diverged:\n--- B ---\n{db_}\n--- C ---\n{dc}"
    );

    // Sanity: the thing they agree on is actually the merged content, not
    // three empty stores agreeing vacuously.
    assert!(da.contains("shared work (b renamed)"), "{da}");
    assert!(da.contains("c filled the body in"), "{da}");
    assert!(
        da.contains("b's own work") && da.contains("c's own work"),
        "{da}"
    );
    assert!(
        !da.contains("needs-resolution"),
        "edits to different fields are not a conflict:\n{da}"
    );
    assert_eq!(count(&a, "task"), 4, "two from A, one each from B and C");
}

/// A ticket edited on one machine and then on the next is not a conflict.
///
/// Case 2: the second machine had already seen the first's value, so the
/// `from` values chain. Treating that as a conflict would flag ordinary
/// sequential work, which would make the marker worthless.
#[test]
fn a_chained_edit_applies_rather_than_conflicting() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a.sqlite"), tmp.path().join("b.sqlite"));
    let ledger = tmp.path().join("ledger");

    init(&a, "AAA");
    init(&b, "BBB");
    ok(&a, &["add", "first title"]);
    export(&a, &ledger);
    merge(&b, &dir_of(&ledger, &a));
    tick();

    ok(&a, &["edit", "AAA-1", "--title", "second title"]);
    export(&a, &ledger);
    tick();
    // B sees A's edit BEFORE making its own, so its `from` is A's `to`.
    merge(&b, &dir_of(&ledger, &a));
    ok(&b, &["edit", "BBB-1", "--title", "third title"]);
    export(&b, &ledger);
    tick();
    merge(&a, &dir_of(&ledger, &b));

    for db in [&a, &b] {
        let dump = canonical_dump(db);
        assert!(
            dump.contains("third title"),
            "the chain did not apply:\n{dump}"
        );
        assert!(
            !dump.contains("needs-resolution"),
            "a sequential edit was reported as concurrent:\n{dump}"
        );
    }
    assert_eq!(canonical_dump(&a), canonical_dump(&b));
}

// ---------------------------------------------------------------------------
// conflict
// ---------------------------------------------------------------------------

/// Two machines editing one field from the same prior value is a real conflict.
///
/// Both values are kept, the field is left where the two sides last agreed, and
/// the ticket is marked. Picking a winner by timestamp is not a fallback here:
/// the two clocks are not comparable, so it would silently discard a real edit.
#[test]
fn a_concurrent_edit_records_both_values_and_marks_the_ticket() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a.sqlite"), tmp.path().join("b.sqlite"));
    let ledger = tmp.path().join("ledger");

    init(&a, "AAA");
    init(&b, "BBB");
    ok(&a, &["add", "the original title"]);
    export(&a, &ledger);
    merge(&b, &dir_of(&ledger, &a));
    tick();

    // Neither has seen the other, so both edits carry the same `from`.
    ok(&a, &["edit", "AAA-1", "--title", "a's title"]);
    ok(&b, &["edit", "BBB-1", "--title", "b's title"]);
    for db in [&a, &b] {
        export(db, &ledger);
    }
    merge(&a, &ledger);
    merge(&b, &ledger);

    for db in [&a, &b] {
        let conn = Connection::open(db).unwrap();
        let mut values: Vec<String> = common::rows(&conn, "SELECT value FROM conflict");
        values.sort();
        assert_eq!(
            values,
            vec!["a's title".to_string(), "b's title".to_string()],
            "both candidate values must survive"
        );
        let base: Vec<String> = common::rows(&conn, "SELECT DISTINCT base FROM conflict");
        assert_eq!(base, vec!["the original title".to_string()]);
        let titles: Vec<String> = common::rows(&conn, "SELECT title FROM task");
        assert_eq!(
            titles,
            vec!["the original title".to_string()],
            "the field must stay at the value both sides agreed on, not a winner"
        );
        let tags: Vec<String> = common::rows(&conn, "SELECT name FROM tag");
        assert!(
            tags.contains(&"needs-resolution".to_string()),
            "the ticket was not marked: {tags:?}"
        );
    }
    assert_eq!(
        canonical_dump(&a),
        canonical_dump(&b),
        "the two machines disagree about the conflict itself"
    );
}

// ---------------------------------------------------------------------------
// the guard
// ---------------------------------------------------------------------------

/// A log that implies an impossible transition is refused, not absorbed.
///
/// Ordering merged events by wall clock is only defensible because every
/// transition is then re-checked against the state the replay actually reached.
/// This removes one `state_change` from a batch so the next one has nothing to
/// move from, and asserts the import says so and writes nothing.
#[test]
fn an_illegal_transition_aborts_the_import_and_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.sqlite");
    let dst = tmp.path().join("restored.sqlite");
    let ledger = tmp.path().join("ledger");

    init(&src, "AAA");
    ok(&src, &["add", "a task"]);
    ok(&src, &["assign", "AAA-1", "--to", "agentA"]);
    ok(&src, &["claim", "AAA-1", "--as", "agentA"]);
    export(&src, &ledger);

    // Lose the `ready` -> `assigned` step. The header still describes the range
    // the file covers, so this is a damaged ledger rather than a renamed one.
    let dir = dir_of(&ledger, &src);
    let path = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| !l.contains("\"to\":\"assigned\""))
        .collect();
    assert_eq!(
        kept.len(),
        text.lines().count() - 1,
        "the doctored ledger dropped the wrong number of lines"
    );
    std::fs::write(&path, kept.join("\n") + "\n").unwrap();

    init(&dst, "ZZZ");
    qp(&dst)
        .args(["import", "--from", dir.to_str().unwrap(), "--restore"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("assigned"))
        .stderr(predicates::str::contains("--force"));
    assert_eq!(
        count(&dst, "task"),
        0,
        "a refused import must leave the store untouched"
    );

    // The escape hatch a damaged-ledger recovery needs: still loud, but it
    // writes what it could rebuild.
    qp(&dst)
        .args([
            "import",
            "--from",
            dir.to_str().unwrap(),
            "--restore",
            "--force",
        ])
        .assert()
        .success()
        .stderr(predicates::str::contains("import anomaly"));
    assert_eq!(count(&dst, "task"), 1);
}

/// A merge must reproduce the source projection exactly when there is nothing
/// to reconcile.
///
/// The wide mutation script from `common` exercises every transition and edge
/// the store has; running it, exporting, and merging into an empty peer holds
/// the merge path to the same diff `export_roundtrip` holds restore to. Only
/// the identity columns differ, which is the whole difference between the modes.
#[test]
fn merging_a_whole_store_into_an_empty_peer_reproduces_its_projection() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst) = (tmp.path().join("src.sqlite"), tmp.path().join("dst.sqlite"));
    let ledger = tmp.path().join("ledger");

    common::init_store(&src);
    common::run_mutation_script(&src);
    export(&src, &ledger);

    // Same prefix, so `display_id` compares too: the peer has minted nothing of
    // its own, so every foreign task lands on the rowid it already had.
    init(&dst, common::PREFIX);
    merge(&dst, &dir_of(&ledger, &src));

    let report = diff_projections(&src, &dst);
    assert!(
        report.is_empty(),
        "a merge into an empty peer lost or invented rows:\n{report}"
    );
}

// ---------------------------------------------------------------------------
// history the log cannot rebuild
// ---------------------------------------------------------------------------

/// Removes a task's creation event, leaving events that reference a task the
/// log can no longer produce.
///
/// This is the shape of every store written before the event log became
/// authoritative: some writes changed the store without recording an event, so
/// the origin state is simply absent. The gap is permanent, which is why it is
/// simulated here rather than waited out — no future fix makes it go away, so
/// the behaviour on top of it has to be right forever.
fn erase_creation_event(db: &Path, task_id: i64) {
    let conn = Connection::open(db).unwrap();
    let n = conn
        .execute(
            "DELETE FROM event WHERE task_id = ? AND kind = 'state_change'
               AND payload LIKE '%\"from\":null%'",
            [task_id],
        )
        .unwrap();
    assert_eq!(
        n, 1,
        "expected exactly one creation event for task {task_id}"
    );
}

fn with_a_gap(db: &Path, ledger: &Path) {
    init(db, "AAA");
    for t in ["one", "two", "three"] {
        ok(db, &["add", &format!("task {t}"), "--tag", "core"]);
    }
    ok(db, &["log", "AAA-2", "note", "a note about two"]);
    erase_creation_event(db, 2);
    export(db, ledger);
}

/// The failure a person meets mid-recovery must diagnose, not leak SQLite.
///
/// `FOREIGN KEY constraint failed: Error code 787` was what came out before:
/// true, useless, and delivered at the moment someone is restoring a backup
/// because something has already gone wrong. The refusal itself is correct —
/// import will not write a projection it cannot faithfully rebuild.
#[test]
fn a_ledger_the_log_cannot_rebuild_is_diagnosed_rather_than_leaked() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.sqlite");
    let dst = tmp.path().join("dst.sqlite");
    let ledger = tmp.path().join("ledger");
    with_a_gap(&src, &ledger);

    init(&dst, "ZZZ");
    let out = qp(&dst)
        .args(["import", "--from", ledger.to_str().unwrap(), "--restore"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();

    assert!(
        !stderr.contains("FOREIGN KEY") && !stderr.contains("787"),
        "the raw SQLite error escaped:\n{stderr}"
    );
    for expected in [
        // what is wrong, and which task
        "no creation event ever minted",
        &format!("{}/2", uuid(&src)),
        // why, and that it is permanent rather than a bug awaiting a fix
        "before the event log became authoritative",
        "cannot be backfilled",
        // what they can do about it
        "--force",
        "do nothing",
    ] {
        assert!(
            stderr.contains(expected),
            "the diagnosis never says {expected:?}:\n{stderr}"
        );
    }
    assert_eq!(count(&dst, "task"), 0, "a refused restore wrote rows");
}

/// `--force` must mean best-effort, not a differently-worded refusal.
///
/// Restoring 2 of 3 tasks with a loud list of what was lost beats restoring
/// none. The events that name the unrebuildable task cannot be written at all —
/// `event.task_id` is a foreign key — so they are dropped by name, and the
/// ledger files still hold them.
#[test]
fn forced_restore_rebuilds_everything_it_can() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.sqlite");
    let dst = tmp.path().join("dst.sqlite");
    let ledger = tmp.path().join("ledger");
    with_a_gap(&src, &ledger);

    init(&dst, "ZZZ");
    let out = qp(&dst)
        .args([
            "import",
            "--from",
            ledger.to_str().unwrap(),
            "--restore",
            "--force",
        ])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();

    assert_eq!(count(&dst, "task"), 2, "a forced restore rebuilt nothing");
    let conn = Connection::open(&dst).unwrap();
    let titles: Vec<String> = common::rows(&conn, "SELECT title FROM task ORDER BY id");
    assert_eq!(titles, vec!["task one", "task three"]);
    // The rest of the store came back whole, not just the task rows.
    assert_eq!(
        common::rows(&conn, "SELECT name FROM tag ORDER BY task_id"),
        vec!["core", "core"]
    );
    assert!(
        stderr.contains("was NOT restored"),
        "the dropped events were not named:\n{stderr}"
    );
    assert!(
        !stderr.contains("FOREIGN KEY"),
        "the raw SQLite error escaped:\n{stderr}"
    );
}

/// A merge must not destroy live rows whose history it cannot rebuild.
///
/// This is the harder half. The task is real, it is in the projection, and a
/// sync that dropped it would delete the owner's data every time it ran. So the
/// default still refuses, and `--force` carries the row across untouched and
/// says which rows those were.
#[test]
fn a_forced_merge_carries_unrebuildable_rows_across_instead_of_dropping_them() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.sqlite");
    let peer = tmp.path().join("peer.sqlite");
    let ledger = tmp.path().join("ledger");
    with_a_gap(&src, &ledger);

    init(&peer, "BBB");
    ok(&peer, &["add", "peer work"]);
    export(&peer, &ledger);
    let peer_dir = dir_of(&ledger, &peer);

    // Default: refused, and the store is exactly as it was.
    let before = canonical_dump(&src);
    qp(&src)
        .args(["import", "--from", peer_dir.to_str().unwrap(), "--merge"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be backfilled"));
    assert_eq!(canonical_dump(&src), before, "a refused merge wrote rows");

    // Forced: the peer's work arrives AND nothing local is lost.
    let out = qp(&src)
        .args([
            "import",
            "--from",
            peer_dir.to_str().unwrap(),
            "--merge",
            "--force",
        ])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();

    assert_eq!(count(&src, "task"), 4, "three local plus the peer's one");
    let conn = Connection::open(&src).unwrap();
    assert_eq!(
        common::rows(&conn, "SELECT title FROM task WHERE id = 2"),
        vec!["task two"],
        "the unrebuildable task was dropped by a merge"
    );
    assert_eq!(
        common::rows(&conn, "SELECT name FROM tag WHERE task_id = 2"),
        vec!["core"],
        "the unrebuildable task kept its row but lost everything hanging off it"
    );
    assert!(
        stderr.contains("carried across UNCHANGED"),
        "the carry-over was not reported:\n{stderr}"
    );
    assert!(
        !stderr.contains("FOREIGN KEY"),
        "the raw SQLite error escaped:\n{stderr}"
    );
}

/// A store written before the log recorded an origin state must still restore.
///
/// Those events are `{"title":...,"to":"ready"}` — no `from` at all. Read
/// naively, every task in such a store looks like one the log never created,
/// and the entire ledger reads as unrestorable — very nearly every task in a
/// real store measured against. The `title` is what distinguishes a creation,
/// because only a creation carries one.
#[test]
fn a_ledger_with_no_origin_states_still_restores() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.sqlite");
    let dst = tmp.path().join("dst.sqlite");
    let ledger = tmp.path().join("ledger");

    init(&src, "AAA");
    ok(&src, &["add", "first"]);
    ok(&src, &["add", "second"]);
    ok(&src, &["assign", "AAA-1", "--to", "agentA"]);
    ok(&src, &["claim", "AAA-1", "--as", "agentA"]);
    // Rewind the log to its pre-fix shape: no event names where it came from.
    Connection::open(&src)
        .unwrap()
        .execute(
            "UPDATE event SET payload = json_remove(payload, '$.from')
              WHERE kind = 'state_change'",
            [],
        )
        .unwrap();
    export(&src, &ledger);

    init(&dst, "ZZZ");
    let out = qp(&dst)
        .args(["import", "--from", ledger.to_str().unwrap(), "--restore"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();

    assert_eq!(count(&dst, "task"), 2, "the legacy ledger restored nothing");
    let conn = Connection::open(&dst).unwrap();
    assert_eq!(
        common::rows(&conn, "SELECT state FROM task ORDER BY id"),
        vec!["running", "ready"],
        "the states did not survive a log with no origin states"
    );
    // The weaker check is reported rather than passed off as a full one: the
    // count rides the success summary on stdout, the consequence is a warning.
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("without a real guard"),
        "the degraded replay was not disclosed:\n{stdout}"
    );
    assert!(
        stderr.contains("materially"),
        "the partial-restore warning is missing:\n{stderr}"
    );
}

/// An edge written before the task it points at is an ordering artifact, not a
/// gap.
///
/// `qp add --part-of` emits the containment edge and the new task's creation in
/// ONE transaction, and the edge goes first. Refusing that would condemn a
/// store for the order two rows of a single commit happened to land in.
#[test]
fn a_creation_referenced_before_it_appears_is_pulled_forward() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.sqlite");
    let dst = tmp.path().join("dst.sqlite");
    let ledger = tmp.path().join("ledger");

    init(&src, "AAA");
    ok(&src, &["add", "the wave", "--tier", "wave"]);
    ok(&src, &["add", "a slice", "--part-of", "AAA-1"]);
    let conn = Connection::open(&src).unwrap();
    let order: Vec<String> = common::rows(&conn, "SELECT kind FROM event ORDER BY id");
    let dep_at = order.iter().position(|k| k == "dep_added").unwrap();
    let births = order
        .iter()
        .enumerate()
        .filter(|(_, k)| *k == "state_change")
        .count();
    assert!(births >= 2 && dep_at < order.len() - 1, "{order:?}");
    drop(conn);
    export(&src, &ledger);

    init(&dst, "ZZZ");
    qp(&dst)
        .args(["import", "--from", ledger.to_str().unwrap(), "--restore"])
        .assert()
        .success();
    assert_eq!(count(&dst, "task"), 2);
    assert_eq!(count(&dst, "dep"), 1, "the containment edge was dropped");
    let report = diff_projections(&src, &dst);
    assert!(report.is_empty(), "{report}");
}

/// A merge must never delete a live row the log has nothing to say about.
///
/// This is the worst failure the whole design can have, because it is silent
/// and it compounds: syncing would delete the owner's tags, edges and
/// descriptions on every run and report success. Measured before the fix, one
/// merge into a real store destroyed the large majority of its tag rows.
///
/// The distinction that makes it safe is between a row the log DELETED and a
/// row it never knew about, so a logged removal must still remove.
#[test]
fn a_merge_keeps_rows_the_log_never_mentioned_and_still_honours_a_logged_removal() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.sqlite");
    let peer = tmp.path().join("peer.sqlite");
    let ledger = tmp.path().join("ledger");

    init(&src, "AAA");
    ok(&src, &["add", "a task", "--description", "the body"]);
    ok(&src, &["add", "another"]);
    ok(&src, &["tag", "AAA-1", "add", "unlogged"]);
    ok(&src, &["tag", "AAA-1", "add", "logged-then-removed"]);
    ok(&src, &["depends", "AAA-2", "--on", "AAA-1"]);
    ok(&src, &["tag", "AAA-1", "rm", "logged-then-removed"]);

    // Simulate the pre-fidelity era: the writes happened, the events did not.
    // `unlogged` keeps its row; the description keeps its value; the dep edge
    // keeps its row — and none of the three is mentioned by any event.
    let conn = Connection::open(&src).unwrap();
    conn.execute(
        "DELETE FROM event WHERE kind = 'tag_added' AND payload LIKE '%unlogged%'",
        [],
    )
    .unwrap();
    conn.execute("DELETE FROM event WHERE kind = 'dep_added'", [])
        .unwrap();
    conn.execute(
        "UPDATE event SET payload = json_remove(payload, '$.description')
          WHERE kind = 'state_change'",
        [],
    )
    .unwrap();
    drop(conn);

    init(&peer, "BBB");
    ok(&peer, &["add", "peer work"]);
    export(&peer, &ledger);
    merge(&src, &dir_of(&ledger, &peer));

    let conn = Connection::open(&src).unwrap();
    assert_eq!(count(&src, "task"), 3, "the peer's task did not arrive");
    assert_eq!(
        common::rows(&conn, "SELECT name FROM tag WHERE task_id = 1"),
        vec!["unlogged"],
        "a merge deleted a tag the log never mentioned, or resurrected one it removed"
    );
    assert_eq!(
        common::rows(&conn, "SELECT description FROM task WHERE id = 1"),
        vec!["the body"],
        "a merge blanked a description the log never carried"
    );
    assert_eq!(
        count(&src, "dep"),
        1,
        "a merge deleted a dep edge the log never mentioned"
    );
}

/// The safety above must not become a licence to ignore the log.
///
/// A row the log DOES speak to stays the rebuild's business: a peer's removal
/// removes, and a peer's addition arrives. Without this the carry-forward would
/// simply pin the projection to whatever it was before the first sync.
#[test]
fn a_merge_still_lets_the_log_remove_what_it_recorded_removing() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a.sqlite"), tmp.path().join("b.sqlite"));
    let ledger = tmp.path().join("ledger");

    init(&a, "AAA");
    init(&b, "BBB");
    ok(&a, &["add", "shared", "--tag", "keep-me"]);
    ok(&a, &["tag", "AAA-1", "add", "doomed"]);
    export(&a, &ledger);
    merge(&b, &dir_of(&ledger, &a));
    let conn = Connection::open(&b).unwrap();
    let mut tags = common::rows(&conn, "SELECT name FROM tag");
    tags.sort();
    assert_eq!(tags, vec!["doomed", "keep-me"]);
    drop(conn);
    tick();

    ok(&a, &["tag", "AAA-1", "rm", "doomed"]);
    export(&a, &ledger);
    merge(&b, &dir_of(&ledger, &a));

    let conn = Connection::open(&b).unwrap();
    assert_eq!(
        common::rows(&conn, "SELECT name FROM tag"),
        vec!["keep-me"],
        "the peer's logged removal did not take effect"
    );
}
