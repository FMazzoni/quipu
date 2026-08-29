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

/// The project every store in this file is a machine of.
///
/// A merge folds a PEER's batches in, and since project identity landed a peer
/// is a store carrying the same project key — not merely one that was handed
/// the same directory. Two plain `qp init`s are two projects, and a scoped
/// merge declines to fold one into the other, which would leave every fixture
/// here quietly merging nothing while still passing. Stores that are strangers
/// on purpose are exercised in `tests/sync.rs`.
const PROJECT: &str = "one-project-under-test";

fn init(db: &Path, prefix: &str) {
    ok(db, &["init", "--prefix", prefix, "--project-key", PROJECT]);
}

/// A store joining the project `of` already belongs to, keeping its own prefix.
fn init_peer_of(db: &Path, prefix: &str, of: &Path) {
    let key: String = Connection::open(of)
        .unwrap()
        .query_row("SELECT value FROM meta WHERE key='project_key'", [], |r| {
            r.get(0)
        })
        .unwrap();
    ok(db, &["init", "--prefix", prefix, "--project-key", &key]);
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
        // Named, not merely counted: the reader's next move is to open the
        // ticket, and a refusal that does not say which one sends them to the
        // whole log instead. A restore adopts the ledger's prefix before replay
        // runs, so the name is the source store's, not this store's.
        .stderr(predicates::str::contains("AAA-1"))
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
    init_peer_of(&dst, common::PREFIX, &src);
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

// ---------------------------------------------------------------------------
// the merge order itself
// ---------------------------------------------------------------------------

/// Rewrites every event in a batch directory to one timestamp.
///
/// Real stores tie on the millisecond often enough — a burst of writes inside a
/// single command does it routinely — but not on demand, and a test that waits
/// for a coincidence tests nothing on the run where the coincidence does not
/// happen. Forcing the tie makes the tie-break the only thing left deciding the
/// sequence.
fn flatten_timestamps(dir: &Path, ts: &str) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let mut out = String::new();
        for line in text.lines() {
            let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
            if v["record"] == "event" {
                v["ts"] = serde_json::Value::String(ts.to_string());
            }
            out.push_str(&serde_json::to_string(&v).unwrap());
            out.push('\n');
        }
        std::fs::write(&path, out).unwrap();
    }
}

/// The same batches, delivered to two peers in opposite orders, one projection.
///
/// This is the whole reason the merge order exists. `peers_converge_...` proves
/// it across three stores that each keep working; this proves the narrower and
/// sharper case — two receivers with *identical* starting points, handed the
/// same set of batches in opposite sequences. Any dependence on arrival order
/// at all shows up here as a diff, with nothing else in the picture that could
/// explain it.
///
/// The two writers interleave in time on purpose. If they took turns in clean
/// blocks the merged order would be a concatenation, and a replay that ignored
/// the order entirely would still pass.
#[test]
fn two_peers_handed_the_same_batches_in_opposite_orders_agree() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a.sqlite"), tmp.path().join("b.sqlite"));
    let (x, y) = (tmp.path().join("x.sqlite"), tmp.path().join("y.sqlite"));
    let ledger = tmp.path().join("ledger");

    for (db, prefix) in [(&a, "AAA"), (&b, "BBB"), (&x, "XXX"), (&y, "YYY")] {
        init(db, prefix);
    }

    // The common ancestor everyone has seen, so neither receiver is being asked
    // to apply events about a task it has never heard of.
    ok(&a, &["add", "shared work"]);
    export(&a, &ledger);
    for db in [&b, &x, &y] {
        merge(db, &dir_of(&ledger, &a));
    }
    tick();

    // Now the two writers diverge, alternating in time. A drives the shared
    // ticket through the state machine; B annotates it and adds work of its
    // own — different projections of the same ticket, which must all survive.
    ok(&a, &["assign", "AAA-1", "--to", "agentA"]);
    tick();
    ok(&b, &["tag", "BBB-1", "add", "prio:high"]);
    tick();
    ok(&a, &["claim", "AAA-1", "--as", "agentA"]);
    tick();
    ok(&b, &["add", "b's own work"]);
    ok(&b, &["relation", "add", "BBB-2", "related-to", "BBB-1"]);
    tick();
    ok(&a, &["complete", "AAA-1", "--as", "agentA"]);
    tick();
    ok(
        &b,
        &["edit", "BBB-1", "--description", "b filled the body in"],
    );
    export(&a, &ledger);
    export(&b, &ledger);

    // The same two batch sets, opposite delivery sequences.
    merge(&x, &dir_of(&ledger, &a));
    merge(&x, &dir_of(&ledger, &b));
    merge(&y, &dir_of(&ledger, &b));
    merge(&y, &dir_of(&ledger, &a));

    let (dx, dy) = (canonical_dump(&x), canonical_dump(&y));
    assert_eq!(
        dx, dy,
        "the same batches in two orders built two stores:\n--- a then b ---\n{dx}\n\
         --- b then a ---\n{dy}"
    );

    // And they agree on the merged content, not on having dropped it. Every
    // clause here comes from a different writer.
    assert!(dx.contains("| done"), "A's transitions are missing:\n{dx}");
    assert!(dx.contains("prio:high"), "B's tag is missing:\n{dx}");
    assert!(
        dx.contains("b filled the body in"),
        "B's edit is missing:\n{dx}"
    );
    assert!(dx.contains("b's own work"), "B's ticket is missing:\n{dx}");
    assert!(
        dx.contains("agentA | claimed=1 | closed=1 | success"),
        "the assignment did not survive the merge:\n{dx}"
    );
}

/// A whole store's log tied on one instant still replays in its own order.
///
/// With every timestamp equal, the first key of the merge order decides
/// nothing and the second decides nothing either — one store wrote all of it.
/// What is left is the event id, and the store's own sequence is the only
/// defensible reading of it: `ready` before `assigned` before `running`,
/// because that is the order the store wrote them in. Without that key the
/// sequence is whatever fed the sort, and the transition guard turns a wrong
/// answer into a refused merge rather than a wrong store.
#[test]
fn a_log_tied_on_a_single_instant_replays_in_the_order_its_store_wrote_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, peer) = (
        tmp.path().join("src.sqlite"),
        tmp.path().join("peer.sqlite"),
    );
    let ledger = tmp.path().join("ledger");

    init(&src, "AAA");
    init(&peer, "BBB");

    // A ticket driven the whole way through the machine, plus a second one
    // demoted and promoted again, so the tied log holds transitions in both
    // directions rather than one monotone run.
    ok(&src, &["add", "the work"]);
    ok(&src, &["add", "the blocker"]);
    ok(&src, &["depends", "AAA-1", "--on", "AAA-2"]);
    ok(&src, &["assign", "AAA-2", "--to", "agentA"]);
    ok(&src, &["claim", "AAA-2", "--as", "agentA"]);
    ok(&src, &["complete", "AAA-2", "--as", "agentA"]);
    ok(&src, &["assign", "AAA-1", "--to", "agentB"]);
    ok(&src, &["claim", "AAA-1", "--as", "agentB"]);
    ok(&src, &["complete", "AAA-1", "--as", "agentB"]);
    export(&src, &ledger);

    let dir = dir_of(&ledger, &src);
    flatten_timestamps(&dir, "2026-01-01T00:00:00.000Z");
    merge(&peer, &dir);

    assert_eq!(
        canonical_dump(&peer),
        canonical_dump(&src),
        "a log whose events all share one timestamp did not replay in its \
         store's own order"
    );
}

/// A move the state machine has no edge for is refused, whatever the ordering.
///
/// Continuity is not legality: this event's `from` is exactly where replay has
/// the task, so the guarded UPDATE matches a row and the projection accepts a
/// hop the machine cannot make. That is the illegal-transition class found in
/// bulk in pre-fidelity logs, and merging a ledger carrying one would import
/// the defect into a healthy store. The refusal has to name the ticket, because
/// the reader's next move is to go and look at it.
#[test]
fn a_transition_the_machine_has_no_edge_for_is_refused_and_names_the_task() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, peer) = (
        tmp.path().join("src.sqlite"),
        tmp.path().join("peer.sqlite"),
    );
    let ledger = tmp.path().join("ledger");

    init(&src, "AAA");
    init(&peer, "BBB");
    ok(&src, &["add", "a task"]);
    ok(&src, &["assign", "AAA-1", "--to", "agentA"]);
    export(&src, &ledger);

    // `ready` -> `done`, in one hop. The task really is `ready` when this
    // event lands, so nothing about *where* it is applied is wrong.
    let dir = dir_of(&ledger, &src);
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""to":"assigned""#), "nothing to doctor");
        std::fs::write(&path, text.replace(r#""to":"assigned""#, r#""to":"done""#)).unwrap();
    }

    qp(&peer)
        .args(["import", "--from", dir.to_str().unwrap(), "--merge"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("BBB-1"))
        .stderr(predicates::str::contains("`ready` -> `done`"))
        .stderr(predicates::str::contains(
            "not an edge of the state machine",
        ));

    assert_eq!(
        count(&peer, "task"),
        0,
        "a refused merge must leave the store untouched"
    );
}

/// Ages a ledger into the shape a pre-fidelity binary wrote.
///
/// Strips every recorded origin, then loses the step into `assigned` the way
/// those writes were lost — without a trace. Both halves matter: an event with
/// no origin is what buys a restore its latitude, and the missing step is what
/// leaves replay somewhere the next event cannot be reached from.
fn age_ledger(dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let kept: Vec<String> = text
            .lines()
            .filter(|l| !l.contains(r#""to":"assigned""#))
            .map(|l| {
                l.replace(r#""from":"ready","#, "")
                    .replace(r#""from":"assigned","#, "")
            })
            .collect();
        std::fs::write(&path, kept.join("\n") + "\n").unwrap();
    }
}

/// A pre-fidelity hop is refused by a merge, whatever it claims.
///
/// An event written before the origin state was recorded says only where the
/// task went. Replay has to supply the origin from where it stands, and a
/// ledger full of those is exactly where illegal hops were found in bulk — a
/// move to `running` from a task nothing ever assigned. There is no recorded
/// pair to check, so the check is against the state replay actually reached,
/// and a merge still refuses rather than folding it into live data.
#[test]
fn a_hop_with_no_recorded_origin_is_refused_by_a_merge() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, peer) = (
        tmp.path().join("src.sqlite"),
        tmp.path().join("peer.sqlite"),
    );
    let ledger = tmp.path().join("ledger");

    init(&src, "AAA");
    init(&peer, "BBB");
    ok(&src, &["add", "a task"]);
    ok(&src, &["assign", "AAA-1", "--to", "agentA"]);
    ok(&src, &["claim", "AAA-1", "--as", "agentA"]);
    export(&src, &ledger);

    let dir = dir_of(&ledger, &src);
    age_ledger(&dir);

    qp(&peer)
        .args(["import", "--from", dir.to_str().unwrap(), "--merge"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("BBB-1"))
        .stderr(predicates::str::contains("records no origin state"))
        .stderr(predicates::str::contains("`running` is not reachable"));
    assert_eq!(count(&peer, "task"), 0);
}

/// The same ledger, restored into an empty store, rebuilds rather than refuses.
///
/// This is the direction where refusing is the destructive answer. A merge has
/// live data to protect and refuses; a restore into an empty store has nothing
/// to protect and hands back an empty store instead of a partial one, which is
/// strictly worse for someone mid-recovery. So the ledger's own admission — a
/// `state_change` recording no origin at all — buys best effort by default, and
/// the move is applied to the destination the event does record. Measured
/// against a real pre-fidelity ledger, refusing these left most of a store's
/// finished tickets sitting in `pending`.
///
/// `--force` is deliberately not the mechanism. A flag every real recovery has
/// to pass is a flag people learn to pass always, which spends the guard's
/// credibility everywhere else.
#[test]
fn the_same_ledger_restored_into_an_empty_store_rebuilds_best_effort() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, peer) = (
        tmp.path().join("src.sqlite"),
        tmp.path().join("peer.sqlite"),
    );
    let ledger = tmp.path().join("ledger");

    init(&src, "AAA");
    ok(&src, &["add", "a task"]);
    ok(&src, &["assign", "AAA-1", "--to", "agentA"]);
    ok(&src, &["claim", "AAA-1", "--as", "agentA"]);
    export(&src, &ledger);
    let dir = dir_of(&ledger, &src);
    age_ledger(&dir);

    init(&peer, "BBB");
    qp(&peer)
        .args(["import", "--from", dir.to_str().unwrap(), "--restore"])
        .assert()
        .success()
        .stderr(predicates::str::contains("APPLIED ANYWAY"))
        .stderr(predicates::str::contains("AAA-1"));

    // The destination the event recorded, not the state replay was stranded
    // in. Anything less means a recovered store where finished work looks
    // untouched.
    let conn = Connection::open(&peer).unwrap();
    let state: String = conn
        .query_row("SELECT state FROM task WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        state, "running",
        "the restore discarded a move whose destination the log records"
    );
    assert_eq!(
        count(&peer, "event"),
        2,
        "every event still lands in the log"
    );
}

/// A modern ledger gets no such latitude: anomalies still refuse a restore.
///
/// The line is where the log's own claims are. Where every event records an
/// origin, an anomaly is a real integrity signal about history we trust, and
/// relaxing it there would make the guard advisory everywhere.
#[test]
fn a_modern_ledger_with_an_anomaly_still_refuses_a_restore() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, peer) = (
        tmp.path().join("src.sqlite"),
        tmp.path().join("peer.sqlite"),
    );
    let ledger = tmp.path().join("ledger");

    init(&src, "AAA");
    ok(&src, &["add", "a task"]);
    ok(&src, &["assign", "AAA-1", "--to", "agentA"]);
    ok(&src, &["claim", "AAA-1", "--as", "agentA"]);
    export(&src, &ledger);

    // Origins left intact — only the step into `assigned` is lost, so the log
    // still claims to know where every move started.
    let dir = dir_of(&ledger, &src);
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let kept: Vec<&str> = text
            .lines()
            .filter(|l| !l.contains(r#""to":"assigned""#))
            .collect();
        std::fs::write(&path, kept.join("\n") + "\n").unwrap();
    }

    init(&peer, "BBB");
    qp(&peer)
        .args(["import", "--from", dir.to_str().unwrap(), "--restore"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--force"));
    assert_eq!(count(&peer, "task"), 0);
}

// ---------------------------------------------------------------------------
// releases replay has to notice
// ---------------------------------------------------------------------------

/// Rewrites a store's log into the shape a pre-fidelity binary wrote a reclaim.
///
/// Two edits, and both are what was found in real ledgers. Every recorded
/// origin goes, because that era did not write one. And the release pair —
/// `-> pending` then the sweep's `-> ready` — collapses into the single
/// `-> ready` hop those binaries emitted, which is the move the state machine
/// has no edge for and the one replay has to survive.
fn age_into_a_direct_reclaim(db: &Path) {
    let conn = Connection::open(db).unwrap();
    conn.execute(
        "DELETE FROM event
          WHERE kind = 'state_change' AND json_extract(payload,'$.via') = 'reclaim'
            AND json_extract(payload,'$.to') = 'pending'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE event SET payload = json_set(payload,'$.via','reclaim')
          WHERE kind = 'state_change' AND json_extract(payload,'$.to') = 'ready'
            AND json_extract(payload,'$.from') = 'pending'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE event SET payload = json_remove(payload,'$.from')
          WHERE kind = 'state_change'",
        [],
    )
    .unwrap();
}

/// A ticket reclaimed the old way, then handed to someone else, still restores.
///
/// The historic release wrote one `running` -> `ready` hop and nothing else, so
/// replay reaches `ready` with the assignment it opened still open. The next
/// `assigned` then opens a second one against a store whose whole point is that
/// a task holds at most one — and what came out was a raw
/// `UNIQUE constraint failed: assignment.task_id` from four frames down, which
/// aborted the entire restore. Two of the stores this was measured against
/// could not be rebuilt from their ledger at all because of six such events.
///
/// The fix is not an arm for `ready`: it is that ANY move out of a held state
/// releases the claim, `assigned` -> `running` excepted.
#[test]
fn a_historic_running_to_ready_release_closes_its_assignment() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.sqlite");
    let dst = tmp.path().join("dst.sqlite");
    let ledger = tmp.path().join("ledger");

    init(&src, "AAA");
    ok(&src, &["add", "a task"]);
    ok(&src, &["assign", "AAA-1", "--to", "agentA"]);
    ok(&src, &["claim", "AAA-1", "--as", "agentA"]);
    tick();
    ok(
        &src,
        &["reclaim", "AAA-1", "--reason", "agent wedged", "--force"],
    );
    tick();
    ok(&src, &["assign", "AAA-1", "--to", "agentB"]);
    ok(&src, &["claim", "AAA-1", "--as", "agentB"]);
    age_into_a_direct_reclaim(&src);
    export(&src, &ledger);

    init(&dst, "ZZZ");
    qp(&dst)
        .args(["import", "--from", ledger.to_str().unwrap(), "--restore"])
        .assert()
        .success();

    assert_eq!(count(&dst, "task"), 1, "the ledger restored no tasks");
    let conn = Connection::open(&dst).unwrap();
    assert_eq!(
        common::rows(&conn, "SELECT state FROM task"),
        vec!["running"],
        "the reclaimed-then-reassigned ticket did not end where the log left it"
    );
    // Two assignments, and only the second one open: the release closed the
    // first, and its outcome names the `via` the event carried.
    assert_eq!(
        common::rows(
            &conn,
            "SELECT agent_id || '/' || CASE WHEN completed_at IS NULL THEN 'open'
                     ELSE COALESCE(outcome,'unrecorded') END FROM assignment ORDER BY id"
        ),
        vec!["agentA/reclaimed", "agentB/open"],
        "the historic release left its assignment open"
    );
}

/// A release replay cannot classify closes the assignment and says so.
///
/// The `via` key is what tells replay which outcome a release recorded, and a
/// log that omits it is a log replay cannot read. Reporting it and leaving the
/// assignment open is the worst of both: the anomaly is raised AND the store is
/// left holding a claim nobody released, which the next `assigned` collides
/// with. So the claim is closed with no outcome — the honest encoding of "it
/// ended, and the log did not say how" — and the anomaly names the task.
#[test]
fn a_release_with_no_via_is_reported_and_still_closes_the_claim() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.sqlite");
    let dst = tmp.path().join("dst.sqlite");
    let ledger = tmp.path().join("ledger");

    init(&src, "AAA");
    ok(&src, &["add", "a task"]);
    ok(&src, &["assign", "AAA-1", "--to", "agentA"]);
    ok(&src, &["claim", "AAA-1", "--as", "agentA"]);
    tick();
    ok(
        &src,
        &["reclaim", "AAA-1", "--reason", "agent wedged", "--force"],
    );
    tick();
    ok(&src, &["assign", "AAA-1", "--to", "agentB"]);
    Connection::open(&src)
        .unwrap()
        .execute(
            "UPDATE event SET payload = json_remove(payload,'$.via')
              WHERE kind = 'state_change' AND json_extract(payload,'$.to') = 'pending'",
            [],
        )
        .unwrap();
    export(&src, &ledger);

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
    assert!(
        stderr.contains("AAA-1") && stderr.contains("via="),
        "the unreadable release was not reported against the task:\n{stderr}"
    );
    let conn = Connection::open(&dst).unwrap();
    assert_eq!(
        common::rows(
            &conn,
            "SELECT agent_id || '/' || CASE WHEN completed_at IS NULL THEN 'open'
                     ELSE COALESCE(outcome,'unrecorded') END FROM assignment ORDER BY id"
        ),
        vec!["agentA/unrecorded", "agentB/open"],
        "the unclassifiable release did not close its claim"
    );
}

/// A second `assigned` over a live claim is reported, not a raw constraint.
///
/// `idx_assign_one_open` is the last thing standing between replay and two open
/// assignments, and what it produces is `UNIQUE constraint failed:
/// assignment.task_id` from inside rusqlite — which aborts the whole restore,
/// writes nothing, and names neither the task nor the events. Someone seeing
/// that is mid-recovery with a store they cannot rebuild and no idea why.
///
/// So the invariant is checked before the INSERT that would break it: the stale
/// claim is closed with no outcome and the anomaly names the ticket. The
/// release arm should already have closed it — this is the backstop for the
/// hops nobody has thought of yet, which is exactly the class this whole
/// ticket came from.
#[test]
fn a_reassignment_over_a_live_claim_is_named_rather_than_a_constraint_error() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.sqlite");
    let dst = tmp.path().join("dst.sqlite");
    let ledger = tmp.path().join("ledger");

    init(&src, "AAA");
    ok(&src, &["add", "a task"]);
    ok(&src, &["assign", "AAA-1", "--to", "agentA"]);
    ok(&src, &["claim", "AAA-1", "--as", "agentA"]);
    tick();
    // A `running` -> `assigned` hop, which the machine has no edge for: the
    // pre-fidelity shape of a handover that skipped the release entirely.
    let conn = Connection::open(&src).unwrap();
    conn.execute(
        "INSERT INTO event(task_id, kind, payload)
         VALUES (1,'state_change','{\"agent_id\":\"agentB\",\"to\":\"assigned\"}')",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE event SET payload = json_remove(payload,'$.from') WHERE kind = 'state_change'",
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
    assert!(
        stderr.contains("AAA-1") && stderr.contains("still open"),
        "the stale claim was not reported against the task:\n{stderr}"
    );
    let conn = Connection::open(&dst).unwrap();
    assert_eq!(
        common::rows(
            &conn,
            "SELECT agent_id || '/' || CASE WHEN completed_at IS NULL THEN 'open'
                     ELSE COALESCE(outcome,'unrecorded') END FROM assignment ORDER BY id"
        ),
        vec!["agentA/unrecorded", "agentB/open"],
        "the handover did not leave exactly one open claim"
    );
}

/// An event kind replay has no projection for is carried, not called damage.
///
/// `qp log` takes the kind as a free-form argument, so `finding`, `review`,
/// `progress` and the rest are ordinary logging, not corruption — and a restore
/// writes every event back verbatim regardless, so nothing is lost by having no
/// arm for them. Raising an anomaly per event put a clean ledger behind
/// `--force` for the crime of writing its own log. The count is still reported,
/// because a structural kind from a newer quipu would land here too.
#[test]
fn a_free_form_log_kind_restores_without_being_called_an_anomaly() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.sqlite");
    let dst = tmp.path().join("dst.sqlite");
    let ledger = tmp.path().join("ledger");

    init(&src, "AAA");
    ok(&src, &["add", "a task"]);
    ok(
        &src,
        &["log", "AAA-1", "finding", "the transport swallows EPIPE"],
    );
    ok(
        &src,
        &[
            "log",
            "AAA-1",
            "spike-result",
            "two of three approaches work",
        ],
    );
    export(&src, &ledger);

    init(&dst, "ZZZ");
    let out = qp(&dst)
        .args(["import", "--from", ledger.to_str().unwrap(), "--restore"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        !stderr.contains("anomaly"),
        "a free-form log kind was reported as an anomaly:\n{stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("finding\u{d7}1") && stdout.contains("spike-result\u{d7}1"),
        "the kinds with no projection were not named:\n{stdout}"
    );
    // Carried verbatim is the whole justification for not refusing: the events
    // are in the restored log, kind and body intact.
    let conn = Connection::open(&dst).unwrap();
    assert_eq!(
        common::rows(
            &conn,
            "SELECT kind FROM event WHERE kind NOT IN ('state_change') ORDER BY id"
        ),
        vec!["finding", "spike-result"],
        "the free-form events did not survive the restore"
    );
}
