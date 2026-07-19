use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn cancel_terminates_task_unblocks_dependents() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["add", "b", "--depends-on", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["cancel", "QP-1", "--reason", "no longer needed"])
        .assert()
        .success();
    // QP-2 should be ready: dep is `cancelled` which counts as resolved.
    qp(&db)
        .args(["assign", "QP-2", "--to", "x"])
        .assert()
        .success();
}

#[test]
fn abandon_returns_running_task_to_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["abandon", "QP-1", "--as", "x"])
        .assert()
        .success();
    // Re-assignable.
    qp(&db)
        .args(["assign", "QP-1", "--to", "y"])
        .assert()
        .success();
}

#[test]
fn reclaim_force_releases_without_agent_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["reclaim", "QP-1", "--reason", "agent unresponsive"])
        .assert()
        .success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "y"])
        .assert()
        .success();
}

#[test]
fn reclaim_returns_to_pending_when_unresolved_dep_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "blocker"]).assert().success(); // QP-1
    qp(&db).args(["add", "main"]).assert().success(); // QP-2
    qp(&db)
        .args(["assign", "QP-2", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-2", "--as", "x"])
        .assert()
        .success();
    // Inject an unresolved dep onto QP-2 while running.
    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1", "--as", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["reclaim", "QP-2", "--reason", "agent unresponsive"])
        .assert()
        .success();
    // Should be pending now — assign rejected.
    qp(&db)
        .args(["assign", "QP-2", "--to", "y"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn assign_rejects_when_stale_open_assignment_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1

    // Inject the row directly: no CLI sequence is known to produce a `ready` task
    // carrying an open assignment (see the `assign` module header, and the
    // `open_assignment_implies_assigned_or_running` test that pins that premise).
    // Reaching the guard therefore requires simulating the corruption it defends
    // against — an out-of-band writer, or a future command that demotes a task
    // without closing its assignment.
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "INSERT INTO assignment(task_id, agent_id) VALUES (1, 'ghost')",
        [],
    )
    .unwrap();
    drop(conn);

    let assert = qp(&db)
        .args(["assign", "QP-1", "--to", "x", "--json"])
        .assert()
        .failure()
        .code(2);
    // Assert the code, not just the exit status: `not_ready` also exits 2, and
    // without this the test would pass while reaching an entirely different guard.
    let v = json_stderr(&assert);
    assert_eq!(v["error"]["code"], "stale_open_assignment");

    // The rejected assign must not have left a second open row behind.
    let conn = rusqlite::Connection::open(&db).unwrap();
    let open: i64 = conn
        .query_row(
            "SELECT count(*) FROM assignment WHERE task_id = 1 AND completed_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(open, 1, "guard must reject, not append");
}

/// QP-142: the one-open-assignment-per-task invariant is enforced by
/// `schema.sql`'s `idx_assign_one_open`, not merely by the agreement of the
/// eight modules that happen to close their assignments.
///
/// This deliberately bypasses the CLI. `assign`'s guard rejects a second open
/// row before SQLite ever sees it, so going through the binary would prove only
/// that the guard works — which is what `assign_rejects_when_stale_open_assignment_exists`
/// already covers. A raw INSERT is the only way to ask whether the *storage
/// layer* would catch a future command that forgets.
#[test]
fn unique_index_rejects_second_open_assignment() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1

    let conn = rusqlite::Connection::open(&db).unwrap();
    // First open row: fine.
    conn.execute(
        "INSERT INTO assignment(task_id, agent_id) VALUES (1, 'one')",
        [],
    )
    .unwrap();
    // Second open row for the same task: rejected by the index.
    let err = conn
        .execute(
            "INSERT INTO assignment(task_id, agent_id) VALUES (1, 'two')",
            [],
        )
        .expect_err("a second open assignment must violate idx_assign_one_open");
    assert!(
        err.to_string().contains("UNIQUE constraint failed"),
        "expected a uniqueness violation, got: {err}"
    );

    // Closing the first row must free the slot — the index covers open rows
    // only, so history is unconstrained and a task can be reassigned forever.
    conn.execute(
        "UPDATE assignment SET completed_at = '2026-01-01T00:00:00.000Z', \
         outcome = 'success' WHERE task_id = 1",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO assignment(task_id, agent_id) VALUES (1, 'two')",
        [],
    )
    .expect("closing the open row must permit a new assignment");
    // And two *closed* rows coexist: NULL-distinctness would have made a plain
    // UNIQUE(task_id, completed_at) constrain exactly the wrong set of rows.
    conn.execute(
        "UPDATE assignment SET completed_at = '2026-01-02T00:00:00.000Z', \
         outcome = 'success' WHERE agent_id = 'two'",
        [],
    )
    .unwrap();
    let closed: i64 = conn
        .query_row(
            "SELECT count(*) FROM assignment WHERE task_id = 1 AND completed_at IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(closed, 2, "closed rows must be exempt from the index");
}

/// QP-142: the index must reach stores that already exist, not just fresh ones.
///
/// This is the failure mode the ticket was most at risk of: `migrate` skips
/// `execute_batch(SCHEMA)` whenever the stamped `schema_version` matches, so an
/// additive `CREATE INDEX IF NOT EXISTS` added without bumping `SCHEMA_VERSION`
/// silently reaches `qp init` in a tempdir and nothing else. A fresh-store test
/// passes either way and would not have caught it.
#[test]
fn existing_store_gains_unique_index_on_migration() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    // Build a populated store, then rewind its version stamp to simulate one
    // created by a binary from before the index existed.
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP INDEX IF EXISTS idx_assign_one_open;
             UPDATE meta SET value = '2' WHERE key = 'schema_version';",
        )
        .unwrap();
    }

    // Any command goes through `open()`, which must migrate the store forward.
    qp(&db).args(["list"]).assert().success();

    let conn = rusqlite::Connection::open(&db).unwrap();
    let has_index: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
              WHERE type='index' AND name='idx_assign_one_open'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_index, 1, "existing store must gain idx_assign_one_open");

    // And it is live on that store, not merely present.
    conn.execute(
        "INSERT INTO assignment(task_id, agent_id) VALUES (1, 'ghost')",
        [],
    )
    .expect_err("the migrated index must reject a second open assignment");
}

/// Pins the premise that makes `assign`'s `stale_open_assignment` branch
/// defensive rather than live: an open assignment row exists only while its
/// task is `assigned` or `running`.
///
/// Since QP-142 `schema.sql` enforces the "at most one open row" half of this
/// structurally, via the `idx_assign_one_open` partial unique index — see
/// `unique_index_rejects_second_open_assignment`. This test still earns its
/// keep because the index cannot express the other half: a task sitting in
/// `ready` or `pending` with exactly one open row satisfies the index perfectly
/// and is still corruption. So this walks each command that moves a task out of
/// `assigned`/`running` and checks the invariant after every step, catching the
/// case the storage layer structurally cannot.
#[test]
fn open_assignment_implies_assigned_or_running() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    for _ in 0..6 {
        qp(&db).args(["add", "t"]).assert().success();
    }

    // Every command that can close an assignment or move a task out of
    // assigned/running, each exercised on its own task.
    let sequences: Vec<Vec<Vec<&str>>> = vec![
        // complete: running -> done
        vec![
            vec!["assign", "QP-1", "--to", "a"],
            vec!["claim", "QP-1", "--as", "a"],
            vec!["complete", "QP-1", "--as", "a"],
        ],
        // abandon: running -> pending -> (refresh_ready) ready
        vec![
            vec!["assign", "QP-2", "--to", "a"],
            vec!["claim", "QP-2", "--as", "a"],
            vec!["abandon", "QP-2", "--as", "a", "--reason", "r"],
        ],
        // reclaim: assigned -> pending -> ready, without an intervening claim
        vec![
            vec!["assign", "QP-3", "--to", "a"],
            vec!["reclaim", "QP-3", "--reason", "r"],
        ],
        // block: running -> pending behind a fresh blocker task
        vec![
            vec!["assign", "QP-4", "--to", "a"],
            vec!["claim", "QP-4", "--as", "a"],
            vec!["block", "QP-4", "--as", "a", "--new", "blocker title"],
        ],
        // cancel: running -> cancelled
        vec![
            vec!["assign", "QP-5", "--to", "a"],
            vec!["claim", "QP-5", "--as", "a"],
            vec!["cancel", "QP-5", "--reason", "r"],
        ],
        // reassign after release: the released task must be assignable again,
        // which is exactly the path that would hit `stale_open_assignment` if
        // the release had failed to close its row.
        vec![
            vec!["assign", "QP-2", "--to", "b"],
            vec!["claim", "QP-2", "--as", "b"],
            vec!["abandon", "QP-2", "--as", "b", "--reason", "r"],
            vec!["assign", "QP-2", "--to", "c"],
        ],
        // dep churn around a released task: refresh_ready promotions must not
        // resurrect a task into `ready` while an assignment is still open.
        vec![
            vec!["assign", "QP-6", "--to", "a"],
            vec!["claim", "QP-6", "--as", "a"],
            vec!["depends", "QP-6", "--on", "QP-3", "--as", "a"],
            vec!["reclaim", "QP-6", "--reason", "r"],
            vec!["depends", "QP-6", "--on", "QP-3", "--rm"],
        ],
    ];

    let check = |label: &str| {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let bad: i64 = conn
            .query_row(
                "SELECT count(*) FROM assignment a JOIN task t ON t.id = a.task_id
                  WHERE a.completed_at IS NULL AND t.state NOT IN ('assigned','running')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bad, 0, "open assignment on a released task after {label}");
        let multi: i64 = conn
            .query_row(
                "SELECT count(*) FROM (SELECT task_id FROM assignment
                   WHERE completed_at IS NULL GROUP BY task_id HAVING count(*) > 1)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(multi, 0, "more than one open assignment after {label}");
    };

    for seq in &sequences {
        for step in seq {
            qp(&db).args(step).assert().success();
            check(&step.join(" "));
        }
    }
}

#[test]
fn version_flag_prints_version() {
    Command::cargo_bin("qp")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("quipu"));
}

#[test]
fn help_lists_core_commands() {
    let assert = Command::cargo_bin("qp")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    for cmd in [
        "init",
        "add",
        "assign",
        "claim",
        "complete",
        "block",
        "cancel",
        "abandon",
        "reclaim",
        "log",
        "tag",
        "relation",
        "tree",
        "timeline",
        "wave",
        "status",
        "list",
        "decisions",
        "wait",
        "watch",
        "install-skills",
        "depends",
        "edit",
        "report",
        "show",
    ] {
        assert!(out.contains(cmd), "help missing `{cmd}`:\n{out}");
    }
}

fn qp(db: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env("QP_DB", db);
    c
}

/// Parse an `assert_cmd` success output's stdout as a bare JSON object
/// (no `{"ok":...}` wrapper expected — success is disjoint from error by
/// stream + exit code, per `outcome::emit`).
fn json_stdout(assert: &assert_cmd::assert::Assert) -> serde_json::Value {
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("stdout was not valid JSON: {e}\nstdout:\n{out}"))
}

/// Parse a failing `assert_cmd` output's stderr as the `{"error": {...}}` envelope.
///
/// Under `--json`, stderr is **JSON Lines**: zero or more `{"warning": {...}}`
/// objects (e.g. `project_uuid_mismatch`, which fires whenever `QP_DB` points
/// somewhere other than the cwd-resolved store — the situation every test here is
/// in), then at most one `{"error": {...}}`. So read the last JSON line rather than
/// parsing the whole buffer. That is the contract, not a workaround. See QP-120.
fn json_stderr(assert: &assert_cmd::assert::Assert) -> serde_json::Value {
    let out = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    let line = out
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON object line found on stderr:\n{out}"));
    serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("stderr line was not valid JSON: {e}\nline:\n{line}"))
}

#[test]
fn timeline_global_includes_all_event_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["complete", "QP-1", "--as", "x", "--decision", "ok"])
        .assert()
        .success();
    let out = qp(&db).args(["timeline", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    let kinds: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"decision") && kinds.iter().filter(|k| **k == "state_change").count() >= 3
    );
}

#[test]
fn decisions_filters_to_decision_events() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["log", "QP-1", "decision", "X", "--auto"])
        .assert()
        .success();
    qp(&db)
        .args(["log", "QP-1", "note", "Y"])
        .assert()
        .success();
    let out = qp(&db).args(["decisions", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
}

#[test]
fn log_writes_event_with_kind_and_body() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "t"]).assert().success();
    qp(&db)
        .args(["log", "QP-1", "decision", "chose B", "--as", "x", "--auto"])
        .assert()
        .success();
    qp(&db)
        .args(["log", "QP-1", "note", "edge case observed"])
        .assert()
        .success();
}

#[test]
fn tag_add_and_rm() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "t"]).assert().success();
    qp(&db)
        .args(["tag", "QP-1", "add", "kind:critique"])
        .assert()
        .success();
    qp(&db)
        .args(["tag", "QP-1", "rm", "kind:critique"])
        .assert()
        .success();
    // Re-removing a tag that doesn't exist should be idempotent (success).
    qp(&db)
        .args(["tag", "QP-1", "rm", "kind:critique"])
        .assert()
        .success();
}

/// A bare `prefix:` is the shape a shell substitution leaves behind when it
/// expands to nothing (`add "commit:$(git rev-parse ...)"` in the wrong cwd).
/// `add` must refuse it; `rm` must still accept it, or rows written before the
/// guard existed could never be cleaned up.
#[test]
fn tag_add_rejects_bare_prefix_but_rm_still_accepts_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "t"]).assert().success();

    qp(&db)
        .args(["tag", "QP-1", "add", "commit:"])
        .assert()
        .failure()
        .stderr(contains("empty value after ':'"));

    // Nothing was stored.
    let out = qp(&db)
        .args(["show", "QP-1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["tags"].as_array().unwrap().len(), 0);

    // A namespaced tag with an actual value is unaffected.
    qp(&db)
        .args(["tag", "QP-1", "add", "commit:4bc299"])
        .assert()
        .success();

    // `rm` accepts the malformed form so pre-guard rows remain removable.
    qp(&db)
        .args(["tag", "QP-1", "rm", "commit:"])
        .assert()
        .success();
}

#[test]
fn relation_add_list_rm() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "root"]).assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db).args(["add", "b"]).assert().success();
    qp(&db)
        .args(["relation", "add", "QP-2", "variant-of", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["relation", "add", "QP-3", "variant-of", "QP-1"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["relation", "list", "QP-1", "--json"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    // incoming variant-of edges from QP-2, QP-3.
    let incoming = v["incoming"].as_array().unwrap();
    assert_eq!(incoming.len(), 2);
    qp(&db)
        .args(["relation", "rm", "QP-2", "variant-of", "QP-1"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["timeline", "QP-2", "--json"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let events: Vec<serde_json::Value> = serde_json::from_str(s.trim()).unwrap();
    assert!(
        events.iter().any(|e| e["kind"] == "relation_removed"),
        "expected a relation_removed event in timeline, got: {events:?}"
    );
}

#[test]
fn relation_add_rm_are_not_silent_in_human_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "root"]).assert().success();
    qp(&db).args(["add", "a"]).assert().success();

    let out = qp(&db)
        .args(["relation", "add", "QP-2", "variant-of", "QP-1"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        s.contains("QP-2") && s.contains("variant-of") && s.contains("QP-1"),
        "relation add was silent/incomplete: {s:?}"
    );

    // Re-adding is a no-op but still reports.
    let out = qp(&db)
        .args(["relation", "add", "QP-2", "variant-of", "QP-1"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("already"), "expected already-linked line: {s:?}");

    let out = qp(&db)
        .args(["relation", "rm", "QP-2", "variant-of", "QP-1"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        s.contains("QP-2") && s.contains("QP-1"),
        "relation rm was silent: {s:?}"
    );
}

#[test]
fn relation_add_rm_json_payloads() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "root"]).assert().success();
    qp(&db).args(["add", "a"]).assert().success();

    // Lowercase / zero-padded input must come back canonicalized (QP-61).
    let out = qp(&db)
        .args(["relation", "add", "qp-002", "supersedes", "qp-1", "--json"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    assert_eq!(v["from_display_id"], "QP-2");
    assert_eq!(v["to_display_id"], "QP-1");
    assert_eq!(v["kind"], "supersedes");
    assert_eq!(v["added"], true);

    // Idempotent re-add reports added:false.
    let out = qp(&db)
        .args(["relation", "add", "QP-2", "supersedes", "QP-1", "--json"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    assert_eq!(v["added"], false);

    let out = qp(&db)
        .args(["relation", "rm", "QP-2", "supersedes", "QP-1", "--json"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    assert_eq!(v["from_display_id"], "QP-2");
    assert_eq!(v["to_display_id"], "QP-1");
    assert_eq!(v["removed"], true);
}

#[test]
fn relation_rm_nonexistent_reports_removed_false() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "root"]).assert().success();
    qp(&db).args(["add", "a"]).assert().success();

    let out = qp(&db)
        .args(["relation", "rm", "QP-2", "variant-of", "QP-1", "--json"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    assert_eq!(v["removed"], false);

    let out = qp(&db)
        .args(["relation", "rm", "QP-2", "variant-of", "QP-1"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("not linked"), "expected not-linked line: {s:?}");

    // A no-op rm must not write a relation_removed event.
    let out = qp(&db)
        .args(["timeline", "QP-2", "--json"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let events: Vec<serde_json::Value> = serde_json::from_str(s.trim()).unwrap();
    assert!(
        !events.iter().any(|e| e["kind"] == "relation_removed"),
        "no-op rm emitted an event: {events:?}"
    );
}

#[test]
fn relation_error_under_json_is_json_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    let out = qp(&db)
        .args([
            "relation",
            "add",
            "QP-999",
            "variant-of",
            "QP-998",
            "--json",
        ])
        .assert()
        .failure();
    let s = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    let line = s.lines().last().unwrap();
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert!(v["error"].is_object(), "expected error envelope: {s:?}");
}

#[test]
fn payload_summary_survives_multibyte_char_at_truncation_boundary() {
    // The fallback arm of summarize_payload truncates the JSON-serialized
    // payload at 80 bytes. Craft a body whose serialized `{"text":"..."}`
    // payload has a multi-byte UTF-8 character (the arrow `→`, 3 bytes)
    // straddling byte offset 80, so a naive `&s[..80]` byte-slice would
    // panic on a non-char-boundary. Use a kind ("note") that isn't
    // special-cased, so it falls into the generic truncation arm.
    let text = format!("{}→{}", "a".repeat(69), "a".repeat(20));
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["log", "QP-1", "note", &text])
        .assert()
        .success();
    qp(&db).args(["timeline", "QP-1"]).assert().success();
    qp(&db).args(["show", "QP-1"]).assert().success();
    qp(&db).arg("report").assert().success();
}

#[test]
fn init_creates_db_and_stamps_project_uuid() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    assert!(db.exists());
    // Idempotent.
    qp(&db).arg("init").assert().success();
}

#[test]
fn init_enables_wal_journal_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    // Open the file directly and ask SQLite what mode it is in. WAL persists
    // in the file header, so a fresh connection sees the same setting.
    let conn = rusqlite::Connection::open(&db).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        mode.to_lowercase(),
        "wal",
        "expected WAL journal mode, got {mode}"
    );
}

#[test]
fn add_creates_task_with_display_id_and_state() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    let out = qp(&db).args(["add", "first", "--json"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    assert_eq!(v["display_id"], "QP-1");
    assert_eq!(v["state"], "ready");
}

#[test]
fn add_with_deps_starts_pending_then_unblocks() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    let out = qp(&db)
        .args(["add", "b", "--depends-on", "QP-1", "--json"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    assert_eq!(v["state"], "pending");
}

#[test]
fn add_with_tags_persists_them() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    let out = qp(&db)
        .args([
            "add",
            "c",
            "--tag",
            "kind:critique",
            "--tag",
            "wave:7",
            "--json",
        ])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    let tags: Vec<String> = v["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert!(tags.contains(&"kind:critique".into()) && tags.contains(&"wave:7".into()));
}

#[test]
fn add_rejects_cycle_on_self_dep() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    // QP-1 → QP-2 → QP-3, then try to add QP-1 dep on QP-3 via a follow-up — but we add depends-on
    // only at creation time in MVP, so cycle is only possible self-on-existing. Skip
    // multi-step cycle; assert the self-dep case via direct error path.
    qp(&db).args(["add", "x"]).assert().success();
    qp(&db)
        .args(["add", "y", "--depends-on", "QP-1"])
        .assert()
        .success();
    // QP-2 depending on QP-1 — fine. Now imagine QP-1 declaring dep on QP-2: not supported via add
    // (you'd need a future `qp dep add` command). For MVP, just verify self-cycle is rejected
    // via an error path; we test would_cycle() indirectly via dep-add in Task 10 if added.
    // Stub assertion: adding with a non-existent dep errors clearly.
    qp(&db)
        .args(["add", "z", "--depends-on", "QP-99"])
        .assert()
        .failure();
}

#[test]
fn tree_renders_tasks_with_state_and_deps() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "root"]).assert().success();
    qp(&db)
        .args(["add", "child", "--depends-on", "QP-1"])
        .assert()
        .success();
    let out = qp(&db).args(["tree"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("QP-1") && s.contains("QP-2"));
}

#[test]
fn status_counts_by_state() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db).args(["add", "b"]).assert().success();
    let out = qp(&db).args(["status", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(v["ready"], 2);
}

#[test]
fn list_embeds_tags_blocked_by_last_event() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["add", "b", "--depends-on", "QP-1", "--tag", "kind:critique"])
        .assert()
        .success();
    let out = qp(&db).args(["list", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    let t2 = v
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["display_id"] == "QP-2")
        .unwrap();
    let tags: Vec<&str> = t2["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert!(tags.contains(&"kind:critique"));
    let blocked: Vec<&str> = t2["blocked_by"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(blocked, vec!["QP-1"]);
    assert!(t2["last_event"].is_object() || t2["last_event"].is_null());
}

#[test]
fn list_filters_by_tag_and_state_and_assignee() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "a", "--tag", "kind:critique"])
        .assert()
        .success();
    qp(&db).args(["add", "b"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "agent-1"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["list", "--tag", "kind:critique", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    let out = qp(&db)
        .args(["list", "--assigned-to", "agent-1", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
}

#[test]
fn mismatched_project_uuid_emits_warning() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let dba = a.path().join("db.sqlite");
    let dbb = b.path().join("db.sqlite");
    qp(&dba).arg("init").assert().success();
    qp(&dbb).arg("init").assert().success();

    // Set up a discoverable .quipu/db.sqlite that's `b`, then run with QP_DB=a from `b`'s cwd.
    let cwd_b = b.path().join("work");
    std::fs::create_dir_all(cwd_b.join(".quipu")).unwrap();
    std::fs::copy(&dbb, cwd_b.join(".quipu/db.sqlite")).unwrap();

    let assert = Command::cargo_bin("qp")
        .unwrap()
        .current_dir(&cwd_b)
        .env("QP_DB", &dba)
        .arg("status")
        .arg("--json")
        .assert()
        .success();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("project_uuid mismatch") || stderr.contains("warning"),
        "expected mismatch warning in stderr:\n{stderr}"
    );
}

#[test]
fn assign_then_claim_happy_path() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "t"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "agent-a"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();
}

#[test]
fn assign_rejects_double_assign() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "t"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "a"])
        .assert()
        .success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "b"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn claim_rejects_wrong_assignee() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "t"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "a"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "b"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn assign_rejects_pending_task() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["add", "b", "--depends-on", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["assign", "QP-2", "--to", "x"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn complete_marks_done_records_decisions_unblocks_deps() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["add", "b", "--depends-on", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    qp(&db)
        .args([
            "complete",
            "QP-1",
            "--as",
            "x",
            "--decision",
            "chose path A",
            "--decision",
            "deferred B",
        ])
        .assert()
        .success();
    // QP-2 should now be assignable (ready).
    qp(&db)
        .args(["assign", "QP-2", "--to", "y"])
        .assert()
        .success();
}

#[test]
fn block_creates_blocker_task_and_demotes_original() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "main"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    qp(&db)
        .args([
            "block",
            "QP-1",
            "--as",
            "alice",
            "--new",
            "obtain prod API key",
        ])
        .assert()
        .success();

    // QP-1 should now be pending (blocked on QP-2). assign should fail.
    qp(&db)
        .args(["assign", "QP-1", "--to", "bob"])
        .assert()
        .failure()
        .code(2);

    // QP-2 should exist as a ready task tagged kind:blocker.
    let out = qp(&db).args(["list", "--json"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("QP-2"));
    assert!(s.contains("kind:blocker"));

    // Resolve the blocker → original auto-thaws.
    qp(&db)
        .args(["assign", "QP-2", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-2", "--as", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["complete", "QP-2", "--as", "alice"])
        .assert()
        .success();
    // QP-1 auto-thaws to ready.
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
}

/// `--tag` replaces the `kind:blocker` default rather than adding to it, so a
/// caller with its own taxonomy doesn't get a foreign convention merged in.
#[test]
fn block_tag_overrides_default_convention() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "main"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    let out = qp(&db)
        .args([
            "block",
            "QP-1",
            "--as",
            "alice",
            "--new",
            "needs review",
            "--tag",
            "kind:review",
            "--tag",
            "urgent",
            "--json",
        ])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("block --json");
    assert_eq!(v["blocker_id"], "QP-2");
    assert_eq!(
        v["blocker_tags"],
        serde_json::json!(["kind:review", "urgent"])
    );

    // The default convention must NOT have been applied alongside the overrides.
    let out = qp(&db)
        .args(["list", "--tag", "kind:blocker", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0, "default tag leaked in");

    // Both overrides are real, filterable tags.
    for t in ["kind:review", "urgent"] {
        let out = qp(&db)
            .args(["list", "--tag", t, "--json"])
            .assert()
            .success();
        let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1, "missing tag {t}");
        assert_eq!(v[0]["display_id"], "QP-2");
    }
}

/// Omitting `--tag` keeps the wave-skill convention working unchanged.
#[test]
fn block_defaults_to_kind_blocker_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "main"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["block", "QP-1", "--as", "alice", "--new", "x", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["blocker_tags"], serde_json::json!(["kind:blocker"]));
}

/// An empty `--tag` is rejected before the transaction, so no orphan task lands.
#[test]
fn block_rejects_empty_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "main"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    qp(&db)
        .args([
            "block", "QP-1", "--as", "alice", "--new", "x", "--tag", "  ",
        ])
        .assert()
        .failure();
    // QP-1 stays running; no blocker task was created.
    let out = qp(&db).args(["list", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["state"], "running");
}

#[test]
fn wave_groups_by_state_and_includes_last_event() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db).args(["add", "b"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    let out = qp(&db).args(["wave", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert!(v["running"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["display_id"] == "QP-1"));
    assert!(v["ready"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["display_id"] == "QP-2"));
    assert!(v["assigned"].is_array());
    assert!(v.get("blocked").is_none(), "blocked group removed");
    assert!(v["pending"].is_array());
}

#[test]
fn wait_returns_when_filter_set_empties() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "a", "--tag", "wave:7"])
        .assert()
        .success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();

    // Start `wait` in the background. No fixed head-start sleep: correctness
    // doesn't depend on whether `wait`'s first poll lands before or after the
    // `complete` below — either way the DB is re-queried fresh each poll, and
    // the generous --timeout-secs is the ceiling that makes this robust under
    // a loaded machine instead of a hand-tuned sleep duration.
    let db2 = db.clone();
    let join = std::thread::spawn(move || {
        Command::cargo_bin("qp")
            .unwrap()
            .env("QP_DB", &db2)
            .args([
                "wait",
                "--tag",
                "wave:7",
                "--state",
                "running",
                "--empty",
                "--interval-ms",
                "20",
                "--timeout-secs",
                "10",
            ])
            .assert()
            .success();
    });
    // Complete the task — wait should return.
    qp(&db)
        .args(["complete", "QP-1", "--as", "x", "--decision", "done"])
        .assert()
        .success();
    join.join().unwrap();
}

#[test]
fn wait_times_out_with_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    qp(&db)
        .args([
            "wait",
            "--state",
            "running",
            "--empty",
            "--interval-ms",
            "20",
            "--timeout-secs",
            "2",
        ])
        .assert()
        .failure()
        .code(3);
}

#[test]
fn wait_cohort_done_does_not_release_on_staggered_claim() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "a", "--tag", "wave:9"])
        .assert()
        .success();
    qp(&db)
        .args(["add", "b", "--tag", "wave:9"])
        .assert()
        .success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["assign", "QP-2", "--to", "y"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-2", "--as", "y"])
        .assert()
        .success();
    // Complete only one of the two — the other is still `running`.
    qp(&db)
        .args(["complete", "QP-1", "--as", "x", "--decision", "done"])
        .assert()
        .success();

    // The barrier must NOT release early: it should time out (exit 3) because
    // QP-2 is still non-terminal.
    qp(&db)
        .args([
            "wait",
            "--tag",
            "wave:9",
            "--cohort-done",
            "--interval-ms",
            "20",
            "--timeout-secs",
            "2",
        ])
        .assert()
        .failure()
        .code(3);

    // Now complete the second task and confirm a clean, prompt return.
    qp(&db)
        .args(["complete", "QP-2", "--as", "y", "--decision", "done"])
        .assert()
        .success();
    qp(&db)
        .args([
            "wait",
            "--tag",
            "wave:9",
            "--cohort-done",
            "--interval-ms",
            "50",
            "--timeout-secs",
            "5",
        ])
        .assert()
        .success();
}

#[test]
fn wait_cohort_done_does_not_release_before_any_claim() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "a", "--tag", "wave:10"])
        .assert()
        .success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    // QP-1 is `assigned`, not yet claimed/running — cohort is not done.
    qp(&db)
        .args([
            "wait",
            "--tag",
            "wave:10",
            "--cohort-done",
            "--interval-ms",
            "20",
            "--timeout-secs",
            "2",
        ])
        .assert()
        .failure()
        .code(3);
}

#[test]
fn wait_cohort_done_errors_on_empty_cohort() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args([
            "wait",
            "--tag",
            "no-such-tag",
            "--cohort-done",
            "--interval-ms",
            "50",
            "--timeout-secs",
            "1",
        ])
        .assert()
        .failure()
        .code(4);
}

#[test]
fn wait_cohort_done_treats_cancelled_as_drained() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "a", "--tag", "wave:11"])
        .assert()
        .success();
    qp(&db)
        .args(["cancel", "QP-1", "--reason", "not needed"])
        .assert()
        .success();
    qp(&db)
        .args([
            "wait",
            "--tag",
            "wave:11",
            "--cohort-done",
            "--interval-ms",
            "50",
            "--timeout-secs",
            "1",
        ])
        .assert()
        .success();
}

#[test]
fn watch_emits_new_events_as_jsonl() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command as PCommand, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "seed"]).assert().success();
    // Start watch in a child. --max-ticks is a generous internal safety net
    // (200 * 20ms = 4s), not the thing that bounds this test's wall time —
    // that's the poll-until-condition loop below, which lets the test finish
    // fast when the machine is idle and stay correct when it's loaded. A
    // fixed small tick budget here previously raced the `add`/`log`
    // subprocess spawns below: under CPU contention those can take longer
    // than the watcher's whole polling window, so the watcher could exit
    // before the new events even landed.
    let bin = assert_cmd::cargo::cargo_bin("qp");
    let mut child = PCommand::new(&bin)
        .env("QP_DB", &db)
        .args([
            "watch",
            "--since",
            "0",
            "--max-ticks",
            "200",
            "--interval-ms",
            "20",
            "--json",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();

    // Stream lines off a background thread so the ceiling below is wall-clock
    // based rather than tied to the watcher's own tick count.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    // Emit a few more events. No fixed pre-sleep: watch polls continuously
    // from --since 0 and will catch these whenever they land relative to its
    // own ticks.
    qp(&db).args(["add", "another"]).assert().success();
    qp(&db)
        .args(["log", "QP-1", "note", "hello"])
        .assert()
        .success();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut lines: Vec<String> = Vec::new();
    while lines.len() < 2 && Instant::now() < deadline {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(200)) {
            if !line.is_empty() {
                lines.push(line);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();

    assert!(lines.len() >= 2, "expected >=2 event lines, got: {lines:?}");
    for line in &lines {
        let _v: serde_json::Value =
            serde_json::from_str(line).expect("each watch line must be valid JSON");
    }
}

#[test]
fn install_skills_symlinks_into_target() {
    let src = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(src.path().join("skills/wave")).unwrap();
    std::fs::write(src.path().join("skills/wave/SKILL.md"), "x").unwrap();

    Command::cargo_bin("qp")
        .unwrap()
        .env("QP_SKILLS_SRC", src.path())
        .args([
            "install-skills",
            "--target",
            target.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(target.path().join("qp-wave/SKILL.md").exists());
}

#[test]
fn install_skills_fails_hard_when_home_unset_and_no_target() {
    let src = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(src.path().join("skills/wave")).unwrap();
    std::fs::write(src.path().join("skills/wave/SKILL.md"), "x").unwrap();

    Command::cargo_bin("qp")
        .unwrap()
        .current_dir(cwd.path())
        .env("QP_SKILLS_SRC", src.path())
        .env_remove("HOME")
        .args(["install-skills"])
        .assert()
        .failure();

    assert!(
        !cwd.path().join(".claude").exists(),
        "install-skills must not create .claude in cwd when HOME is unset"
    );
}

#[test]
fn wave_lists_pending_tasks_that_have_unresolved_deps() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1 ready
    qp(&db)
        .args(["add", "b", "--depends-on", "QP-1"])
        .assert()
        .success(); // QP-2 pending
    let out = qp(&db).args(["wave", "--json"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    assert!(
        v.get("blocked").is_none(),
        "should not have `blocked` group"
    );
    let pending = v["pending"].as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["display_id"], "QP-2");
}

#[test]
fn wave_excludes_pending_task_without_unresolved_deps() {
    // The `pending` state normally implies an unresolved dep — refresh_ready
    // promotes to `ready` as soon as the last one clears — so this state
    // (pending with zero unresolved deps) isn't reachable through the CLI.
    // Force it directly to prove wave.rs's pending arm still carries its
    // extra unresolved-dep predicate rather than showing every pending task.
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1 ready
    qp(&db)
        .args(["add", "b", "--depends-on", "QP-1"])
        .assert()
        .success(); // QP-2 pending, unresolved dep on QP-1

    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "INSERT INTO task(display_id, title, state) VALUES ('QP-3', 'c', 'pending')",
        [],
    )
    .unwrap();
    drop(conn);

    let out = qp(&db).args(["wave", "--json"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    let pending = v["pending"].as_array().unwrap();
    assert_eq!(
        pending.len(),
        1,
        "QP-3 has no unresolved dep and must not appear"
    );
    assert_eq!(pending[0]["display_id"], "QP-2");
}

#[test]
fn add_with_custom_prefix_uses_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db)
        .args(["init", "--prefix", "ACME"])
        .assert()
        .success();
    let out = qp(&db).args(["add", "first", "--json"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    assert_eq!(v["display_id"], "ACME-1");
}

#[test]
fn init_with_invalid_prefix_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db)
        .args(["init", "--prefix", "qp"])
        .assert()
        .failure()
        .code(2);
    qp(&db)
        .args(["init", "--prefix", "TOOLONG"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn init_prefix_is_immutable_after_first_init() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db)
        .args(["init", "--prefix", "ACME"])
        .assert()
        .success();
    // Second init with a different prefix should be silently idempotent —
    // prefix is NOT changed.
    qp(&db)
        .args(["init", "--prefix", "OTHER"])
        .assert()
        .success();
    let out = qp(&db).args(["add", "x", "--json"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("ACME-1"), "prefix should remain ACME, got: {s}");
}

// ─── Pattern C — Slice A appended tests (use QP-<n> ids) ───

#[test]
fn depends_links_two_ready_tasks_no_agent_required() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db).args(["add", "b"]).assert().success();
    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1"])
        .assert()
        .success();
    // QP-2 should now be pending (has an unresolved dep on QP-1).
    qp(&db)
        .args(["assign", "QP-2", "--to", "x"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn depends_rejects_self_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-1"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn depends_rejects_transitive_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["add", "b", "--depends-on", "QP-1"])
        .assert()
        .success();
    // Adding QP-1 → depends-on → QP-2 would close the loop.
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn depends_on_running_task_requires_matching_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db).args(["add", "b"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    // No --as flag → rejected.
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2"])
        .assert()
        .failure()
        .code(2);
    // Wrong --as → rejected.
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2", "--as", "bob"])
        .assert()
        .failure()
        .code(2);
    // Matching --as → success.
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2", "--as", "alice"])
        .assert()
        .success();
}

#[test]
fn depends_rm_can_unblock_pending_task() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["add", "b", "--depends-on", "QP-1"])
        .assert()
        .success();
    // QP-2 is pending.
    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1", "--rm"])
        .assert()
        .success();
    // Now assignable.
    qp(&db)
        .args(["assign", "QP-2", "--to", "x"])
        .assert()
        .success();
}

#[test]
fn abandon_returns_to_ready_when_no_unresolved_deps() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["abandon", "QP-1", "--as", "x"])
        .assert()
        .success();
    // Re-assignable: state must be 'ready'.
    qp(&db)
        .args(["assign", "QP-1", "--to", "y"])
        .assert()
        .success();
}

#[test]
fn abandon_returns_to_pending_when_unresolved_dep_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "blocker"]).assert().success(); // QP-1
    qp(&db).args(["add", "main"]).assert().success(); // QP-2
    qp(&db)
        .args(["assign", "QP-2", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-2", "--as", "x"])
        .assert()
        .success();
    // Inject an unresolved dep onto QP-2 while running.
    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1", "--as", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["abandon", "QP-2", "--as", "x"])
        .assert()
        .success();
    // Should be pending now — assign rejected.
    qp(&db)
        .args(["assign", "QP-2", "--to", "y"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn block_rejects_wrong_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "main"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["block", "QP-1", "--as", "bob", "--new", "x"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn depends_demote_emits_state_change_event() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db).args(["add", "b"]).assert().success();
    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["timeline", "QP-2", "--json"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    let kinds: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"dep_added"),
        "expected dep_added in {kinds:?}"
    );
    assert!(
        kinds.contains(&"state_change"),
        "expected state_change in {kinds:?}"
    );
}

#[test]
fn edit_requires_at_least_one_field() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db).args(["edit", "QP-1"]).assert().failure().code(1);
}

#[test]
fn edit_updates_title_and_emits_edit_event() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "old title"]).assert().success();
    qp(&db)
        .args(["edit", "QP-1", "--title", "new title"])
        .assert()
        .success();
    let out = qp(&db).args(["list", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(v.as_array().unwrap()[0]["title"], "new title");
    let tl = qp(&db)
        .args(["timeline", "QP-1", "--json"])
        .assert()
        .success();
    let tv: serde_json::Value =
        serde_json::from_str(std::str::from_utf8(&tl.get_output().stdout).unwrap().trim()).unwrap();
    assert!(
        tv.as_array().unwrap().iter().any(|e| e["kind"] == "edit"),
        "expected an `edit` event in timeline"
    );
}

#[test]
fn edit_no_op_skips_event() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["edit", "QP-1", "--title", "a"])
        .assert()
        .success();
    let tl = qp(&db)
        .args(["timeline", "QP-1", "--json"])
        .assert()
        .success();
    let tv: serde_json::Value =
        serde_json::from_str(std::str::from_utf8(&tl.get_output().stdout).unwrap().trim()).unwrap();
    assert!(
        !tv.as_array().unwrap().iter().any(|e| e["kind"] == "edit"),
        "no-op edit should not emit event"
    );
}

#[test]
fn edit_can_clear_tier_with_empty_string() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "a", "--tier", "p1"])
        .assert()
        .success();
    qp(&db)
        .args(["edit", "QP-1", "--tier", ""])
        .assert()
        .success();
    let out = qp(&db).args(["list", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    // tier becomes null/absent in JSON.
    assert!(
        v.as_array().unwrap()[0]["tier"].is_null()
            || v.as_array().unwrap()[0].get("tier").is_none()
    );
}

#[test]
fn add_with_description_stores_and_lists_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "a", "--description", "long form scope notes"])
        .assert()
        .success();
    let out = qp(&db).args(["list", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(
        v.as_array().unwrap()[0]["description"],
        "long form scope notes"
    );
}

#[test]
fn edit_during_running_state_allowed() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    // Scope refinement mid-flight should be allowed.
    qp(&db)
        .args(["edit", "QP-1", "--description", "scope refined"])
        .assert()
        .success();
}

#[test]
fn edit_rejected_on_done_task() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["complete", "QP-1", "--as", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["edit", "QP-1", "--title", "no go"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn edit_rejected_on_cancelled_task() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["cancel", "QP-1", "--reason", "obsolete"])
        .assert()
        .success();
    qp(&db)
        .args(["edit", "QP-1", "--title", "no go"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn edit_rejects_empty_title() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["edit", "QP-1", "--title", ""])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn decisions_auto_only_filters_non_auto() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["log", "QP-1", "decision", "auto-decided", "--auto"])
        .assert()
        .success();
    qp(&db)
        .args(["log", "QP-1", "decision", "human-decided"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["decisions", "--auto-only", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v.as_array().unwrap()[0]["payload"]["text"], "auto-decided");
}

#[test]
fn decisions_json_and_auto_only_json_share_row_shape() {
    // QP-68: --json and --json --auto-only used to take different code
    // paths (timeline delegation vs. a hand-rolled query). Now both go
    // through store::events, so their row shapes must match exactly.
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["log", "QP-1", "decision", "auto-decided", "--auto"])
        .assert()
        .success();
    qp(&db)
        .args(["log", "QP-1", "decision", "human-decided"])
        .assert()
        .success();

    let all = qp(&db).args(["decisions", "--json"]).assert().success();
    let all_v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&all.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    let auto = qp(&db)
        .args(["decisions", "--json", "--auto-only"])
        .assert()
        .success();
    let auto_v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&auto.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();

    assert_eq!(all_v.as_array().unwrap().len(), 2);
    assert_eq!(auto_v.as_array().unwrap().len(), 1);

    let mut all_keys: Vec<&str> = all_v.as_array().unwrap()[0]
        .as_object()
        .unwrap()
        .keys()
        .map(|s| s.as_str())
        .collect();
    let mut auto_keys: Vec<&str> = auto_v.as_array().unwrap()[0]
        .as_object()
        .unwrap()
        .keys()
        .map(|s| s.as_str())
        .collect();
    all_keys.sort();
    auto_keys.sort();
    assert_eq!(all_keys, auto_keys, "row shape differs between code paths");
    assert_eq!(
        all_keys,
        vec!["agent_id", "id", "kind", "payload", "task", "ts"]
    );
}

#[test]
fn resolve_path_finds_store_from_worktree() {
    // Simulate: a main repo with .quipu/, and a sibling "worktree" dir
    // whose only git-link points back at the main repo. qp run from inside
    // the worktree should find the main repo's .quipu/.
    use std::process::Command as PCommand;
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("repo-main");
    std::fs::create_dir(&main).unwrap();
    // Init a git repo + initial commit so worktree-add works.
    PCommand::new("git")
        .args(["init", "-q"])
        .current_dir(&main)
        .status()
        .unwrap();
    PCommand::new("git")
        .args(["commit", "--allow-empty", "-q", "-m", "init"])
        .current_dir(&main)
        .status()
        .unwrap();
    // Initialize a qp store in the main repo.
    Command::cargo_bin("qp")
        .unwrap()
        .current_dir(&main)
        .arg("init")
        .assert()
        .success();
    // Create a worktree as a sibling.
    let wt = tmp.path().join("repo-wt");
    PCommand::new("git")
        .args([
            "worktree",
            "add",
            "-q",
            "-b",
            "tmpbranch",
            wt.to_str().unwrap(),
        ])
        .current_dir(&main)
        .status()
        .unwrap();
    // Without QP_DB, running qp from the worktree should still work
    // and create the task in the MAIN repo's store.
    Command::cargo_bin("qp")
        .unwrap()
        .current_dir(&wt)
        .args(["add", "from-worktree"])
        .assert()
        .success();
    // Verify the task landed in the main store.
    let out = Command::cargo_bin("qp")
        .unwrap()
        .current_dir(&main)
        .args(["list", "--json"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        s.contains("from-worktree"),
        "task should be in main store: {s}"
    );
}

#[test]
fn log_auto_attributes_to_running_assignee() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    // No --as — should still attribute to alice because QP-1 is running.
    qp(&db)
        .args(["log", "QP-1", "note", "from inside the run"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["timeline", "QP-1", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    let note = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "note")
        .expect("note event present");
    assert_eq!(
        note["agent_id"], "alice",
        "log should auto-attribute to running assignee"
    );
}

#[test]
fn log_does_not_auto_attribute_when_not_running() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    // Task is ready, never assigned/claimed.
    qp(&db)
        .args(["log", "QP-1", "note", "from the void"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["timeline", "QP-1", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    let note = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "note")
        .expect("note event present");
    // agent_id should be absent or null.
    assert!(
        note.get("agent_id").is_none_or(|v| v.is_null()),
        "log should NOT auto-attribute on non-running task; got {:?}",
        note["agent_id"]
    );
}

#[test]
fn log_explicit_as_always_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    // Explicit --as overrides the auto default.
    qp(&db)
        .args(["log", "QP-1", "note", "orchestrator log", "--as", "orch"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["timeline", "QP-1", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    let note = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "note")
        .expect("note event present");
    assert_eq!(note["agent_id"], "orch");
}

#[test]
fn init_default_tag_is_applied_to_subsequent_adds() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db)
        .args(["init", "--default-tag", "harness:claude-code"])
        .assert()
        .success();
    let out = qp(&db).args(["add", "t", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    let tags: Vec<&str> = v["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(tags.contains(&"harness:claude-code"));
}

#[test]
fn init_default_tag_is_additive_across_reinits() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db)
        .args(["init", "--default-tag", "a"])
        .assert()
        .success();
    qp(&db)
        .args(["init", "--default-tag", "b"])
        .assert()
        .success();
    let out = qp(&db).args(["add", "t", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    let tags: Vec<&str> = v["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(tags.contains(&"a") && tags.contains(&"b"));
}

#[test]
fn default_tag_dedupes_against_explicit_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db)
        .args(["init", "--default-tag", "foo"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["add", "t", "--tag", "foo", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    let tags: Vec<&str> = v["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert_eq!(tags.iter().filter(|t| **t == "foo").count(), 1);
}

#[test]
fn list_assigned_to_supports_glob_pattern() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db).args(["add", "b"]).assert().success();
    qp(&db).args(["add", "c"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "claude-code:agent-x"])
        .assert()
        .success();
    qp(&db)
        .args(["assign", "QP-2", "--to", "claude-code:agent-y"])
        .assert()
        .success();
    qp(&db)
        .args(["assign", "QP-3", "--to", "cli:nando"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["list", "--assigned-to", "claude-code:*", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    let ids: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["display_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"QP-1") && ids.contains(&"QP-2"));
    assert!(!ids.contains(&"QP-3"));
}

#[test]
fn list_assigned_to_exact_match_still_works() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db).args(["add", "b"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["assign", "QP-2", "--to", "alicia"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["list", "--assigned-to", "alice", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    let ids: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["display_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["QP-1"]);
}

/// Reads `list --json` and returns the display_ids in order.
fn list_ids(out: &assert_cmd::assert::Assert) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    v.as_array()
        .unwrap()
        .iter()
        .map(|t| t["display_id"].as_str().unwrap().to_string())
        .collect()
}

/// QP-49's motivating case: commit SHAs were normalised to 6 chars purely
/// because `--tag` could not prefix-match. With GLOB the 6/7-char mix that
/// actually exists in the dogfood db is reachable by one pattern.
#[test]
fn list_tag_supports_glob_pattern() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "a", "--tag", "commit:5c5b30"])
        .assert()
        .success();
    qp(&db)
        .args(["add", "b", "--tag", "commit:5c5b30f"])
        .assert()
        .success();
    qp(&db)
        .args(["add", "c", "--tag", "commit:abcdef"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["list", "--tag", "commit:5c5b30*", "--json"])
        .assert()
        .success();
    assert_eq!(list_ids(&out), vec!["QP-1", "QP-2"]);
}

/// The compatibility half of QP-49: a wildcard-free pattern must stay an
/// exact match, so `--tag wave:1` cannot start dragging in `wave:11`.
#[test]
fn list_tag_without_wildcard_is_exact() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "a", "--tag", "wave:1"])
        .assert()
        .success();
    qp(&db)
        .args(["add", "b", "--tag", "wave:11"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["list", "--tag", "wave:1", "--json"])
        .assert()
        .success();
    assert_eq!(list_ids(&out), vec!["QP-1"]);
}

/// Repeated `--tag` flags AND together — globbing must not turn the
/// conjunction into a union.
#[test]
fn list_tag_globs_and_together() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "a", "--tag", "kind:bug", "--tag", "wave:3"])
        .assert()
        .success();
    qp(&db)
        .args(["add", "b", "--tag", "kind:bug"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["list", "--tag", "kind:*", "--tag", "wave:*", "--json"])
        .assert()
        .success();
    assert_eq!(list_ids(&out), vec!["QP-1"]);
}

/// QP-41, closed as accepted rather than fixed: `[` and `]` are GLOB
/// character-class syntax, so a bracketed agent_id is not matchable by
/// pasting it in verbatim — `claude-code[worker]` reads as "claude-cod"
/// followed by one character from a class. This test pins the behaviour so
/// that adding escaping later is a deliberate decision rather than a silent
/// one, and pins the workaround (`?` in the bracket positions). `--tag`
/// behaves identically because both predicates speak one language.
#[test]
fn list_glob_brackets_are_character_classes_not_literals() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "claude-code[worker]"])
        .assert()
        .success();

    // Verbatim paste does not match: the brackets are consumed as syntax.
    let out = qp(&db)
        .args(["list", "--assigned-to", "claude-code[worker]", "--json"])
        .assert()
        .success();
    assert!(list_ids(&out).is_empty());

    // The documented workaround: `?` matches the bracket characters.
    let out = qp(&db)
        .args(["list", "--assigned-to", "claude-code?worker?", "--json"])
        .assert()
        .success();
    assert_eq!(list_ids(&out), vec!["QP-1"]);

    // A tag with brackets behaves the same way — one language, not two.
    qp(&db)
        .args(["add", "b", "--tag", "ref:[x]"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["list", "--tag", "ref:[x]", "--json"])
        .assert()
        .success();
    assert!(list_ids(&out).is_empty());
    let out = qp(&db)
        .args(["list", "--tag", "ref:?x?", "--json"])
        .assert()
        .success();
    assert_eq!(list_ids(&out), vec!["QP-2"]);
}

#[test]
fn status_shows_all_states_including_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    let out = qp(&db).arg("status").assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    for state in [
        "pending",
        "ready",
        "assigned",
        "running",
        "done",
        "cancelled",
    ] {
        assert!(s.contains(state), "status missing `{state}`:\n{s}");
    }
}

#[test]
fn wave_human_mode_says_nothing_in_flight_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    let out = qp(&db).arg("wave").assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        s.contains("nothing in flight"),
        "wave missing guidance:\n{s}"
    );
}

#[test]
fn decisions_human_mode_renders_readable_text() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["log", "QP-1", "decision", "the decision body", "--auto"])
        .assert()
        .success();
    let out = qp(&db).arg("decisions").assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("the decision body"), "missing text:\n{s}");
    assert!(!s.contains("{\""), "raw JSON leaked:\n{s}");
}

#[test]
fn timeline_human_mode_renders_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["complete", "QP-1", "--as", "x", "--decision", "all-good"])
        .assert()
        .success();
    let out = qp(&db).arg("timeline").assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    for line in s.lines() {
        assert!(
            !line.starts_with('{'),
            "line starts with JSON brace: {line}"
        );
    }
    assert!(s.contains("state_change"), "missing state_change:\n{s}");
    assert!(s.contains("decision"), "missing decision:\n{s}");
    assert!(s.contains("all-good"), "missing decision text:\n{s}");
}

#[test]
fn list_human_mode_has_header_row() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    let out = qp(&db).arg("list").assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let first = s.lines().next().unwrap_or("");
    assert!(
        first.starts_with("ID"),
        "expected header starting with ID, got: {first}"
    );
    assert!(
        first.contains("STATE")
            && first.contains("AGENT")
            && first.contains("TAGS")
            && first.contains("TITLE"),
        "header missing columns: {first}"
    );
}

#[test]
fn tree_uses_display_id_format_for_dep_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["add", "b", "--depends-on", "QP-1"])
        .assert()
        .success();
    let out = qp(&db).arg("tree").assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        s.contains("QP-1") && s.contains("QP-2"),
        "tree missing display ids:\n{s}"
    );
    assert!(
        s.contains("[QP-1]"),
        "dep ref not in display-id format:\n{s}"
    );
    assert!(!s.contains("[T1]"), "old T-prefix format leaked:\n{s}");
}

#[test]
fn tree_with_task_arg_filters_to_subtree() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "unrelated-1"]).assert().success(); // QP-1
    qp(&db).args(["add", "unrelated-2"]).assert().success(); // QP-2
    qp(&db).args(["add", "unrelated-3"]).assert().success(); // QP-3
    qp(&db).args(["add", "leaf-a"]).assert().success(); // QP-4
    qp(&db).args(["add", "leaf-b"]).assert().success(); // QP-5
    qp(&db)
        .args([
            "add",
            "root",
            "--depends-on",
            "QP-4",
            "--depends-on",
            "QP-5",
        ])
        .assert()
        .success(); // QP-6
    let out = qp(&db).args(["tree", "QP-6"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        s.contains("QP-6") && s.contains("QP-4") && s.contains("QP-5"),
        "subtree missing root/deps:\n{s}"
    );
    assert!(
        !s.contains("unrelated-1") && !s.contains("unrelated-2") && !s.contains("unrelated-3"),
        "subtree leaked unrelated tasks:\n{s}"
    );
}

#[test]
fn show_renders_title_description_and_events() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "ticket title", "--description", "long form notes"])
        .assert()
        .success();
    let out = qp(&db).args(["show", "QP-1"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("ticket title"));
    assert!(s.contains("long form notes"));
    assert!(s.contains("QP-1"));
}

#[test]
fn show_json_includes_recent_events() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    let out = qp(&db).args(["show", "QP-1", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(v["display_id"], "QP-1");
    assert!(v["recent_events"].is_array());
    assert!(v["recent_events"].as_array().unwrap().len() >= 2); // ready + assigned
}

/// QP-154: after `abandon` the task has no holder but keeps naming its last
/// assignee. `list`, the `show` header, and the `show` body must all report
/// that one fact identically — the header used to omit it, so a reader
/// cross-checking the two commands concluded `list` was wrong.
#[test]
fn show_header_and_body_agree_on_agent_after_abandon() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "t"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["abandon", "QP-1", "--as", "alice"])
        .assert()
        .success();

    let out = qp(&db).args(["show", "QP-1"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let header = s.lines().next().unwrap();
    assert!(
        header.contains("alice"),
        "header must name the latest assignee:\n{s}"
    );
    assert!(
        s.contains("  agent: alice"),
        "body must name the latest assignee:\n{s}"
    );

    // ...and `list` must say the same thing.
    let out = qp(&db).args(["list"]).assert().success();
    let l = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(l.contains("alice"), "list disagrees with show:\n{l}");
}

/// QP-156: the `show` header carries exactly `list`'s columns in `list`'s order
/// (id, state, agent, tags) and nothing else. `tier` is a labelled body field.
/// The header must therefore have the same shape whether or not a tier is set —
/// an unlabelled column `list` does not have is what made a `-` read as "no
/// agent" in QP-154.
#[test]
fn show_header_mirrors_list_columns_and_tier_is_labelled() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "no tier"]).assert().success();
    qp(&db)
        .args(["add", "with tier", "--tier", "p1"])
        .assert()
        .success();
    for t in ["QP-1", "QP-2"] {
        qp(&db)
            .args(["assign", t, "--to", "alice"])
            .assert()
            .success();
    }

    let header = |t: &str| -> (String, String) {
        let out = qp(&db).args(["show", t]).assert().success();
        let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        (s.lines().next().unwrap().to_string(), s)
    };

    let (h1, s1) = header("QP-1");
    let (h2, s2) = header("QP-2");

    // Four columns, tier-independent.
    let cols = |h: &str| h.split("  ").filter(|c| !c.is_empty()).count();
    assert_eq!(cols(&h1), 4, "header must be id/state/agent/tags:\n{s1}");
    assert_eq!(cols(&h2), 4, "tier must not add a column:\n{s2}");
    assert!(h1.starts_with("QP-1  assigned  alice  "), "{s1}");
    assert!(h2.starts_with("QP-2  assigned  alice  "), "{s2}");
    // The tier value never appears in the header, only in the labelled block.
    assert!(!h2.contains("p1"), "tier leaked into the header:\n{s2}");
    assert!(s2.contains("  tier: p1"), "tier must be labelled:\n{s2}");
    assert!(!s1.contains("tier:"), "no tier line when unset:\n{s1}");

    // Column order matches `list`'s header exactly.
    let out = qp(&db).args(["list"]).assert().success();
    let l = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_eq!(
        l.lines().next().unwrap(),
        "ID\tSTATE\tAGENT\tTAGS\tTITLE",
        "list header changed; show's header must be kept in step"
    );

    // `--json` is the supported contract and still carries tier.
    let out = qp(&db).args(["show", "QP-2", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(v["tier"], "p1");
}

/// QP-152: `--json` is complete (matching `report --ticket`); human mode caps
/// for readability but must announce what it dropped.
#[test]
fn show_json_event_tail_is_uncapped_and_human_signals_truncation() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "t"]).assert().success();
    // 1 state_change + 20 notes = 21 events, comfortably over the human cap.
    for i in 0..20 {
        qp(&db)
            .args(["log", "QP-1", "note", &format!("note {i}")])
            .assert()
            .success();
    }

    let out = qp(&db).args(["show", "QP-1", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    let json_events = v["recent_events"].as_array().unwrap().len();
    assert_eq!(json_events, 21, "--json must not truncate: {json_events}");

    // The same completeness contract `report --ticket` already honours.
    let out = qp(&db)
        .args(["report", "--json", "--ticket", "QP-1"])
        .assert()
        .success();
    let r: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(
        r["events"].as_array().unwrap().len(),
        json_events,
        "show --json and report --ticket disagree on event count"
    );

    let out = qp(&db).args(["show", "QP-1"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let shown = s.lines().filter(|l| l.contains("note ")).count();
    assert_eq!(shown, 10, "human mode should cap the tail:\n{s}");
    assert!(
        s.contains("Recent events (10 of 21):"),
        "human header must state the total:\n{s}"
    );
    assert!(
        s.contains("… 11 older (qp timeline QP-1)"),
        "human mode must signal the truncation and where to get the rest:\n{s}"
    );
}

#[test]
fn show_zero_padded_ref_renders_canonical_display_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "ticket title"]).assert().success(); // QP-1

    let out = qp(&db).args(["show", "qp-001"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        s.contains("QP-1"),
        "expected canonical QP-1 in output:\n{s}"
    );
    assert!(!s.contains("qp-001"), "raw input echoed back:\n{s}");

    let out = qp(&db)
        .args(["show", "qp-001", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(v["display_id"], "QP-1");
}

#[test]
fn show_blocked_by_sorts_numerically_not_lexically() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    // QP-1 .. QP-10: ten single-purpose blocker tasks.
    for i in 1..=10 {
        qp(&db)
            .args(["add", &format!("blocker {i}")])
            .assert()
            .success();
    }
    // QP-11: depends on all ten, in an order that would sort wrong
    // lexically ("QP-10" < "QP-2" as strings) if not ordered by t.id.
    let mut args = vec!["add".to_string(), "dependent".to_string()];
    for i in 1..=10 {
        args.push("--depends-on".to_string());
        args.push(format!("QP-{i}"));
    }
    qp(&db).args(&args).assert().success();

    let out = qp(&db).args(["show", "QP-11", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout)
            .unwrap()
            .trim(),
    )
    .unwrap();
    let blocked: Vec<&str> = v["blocked_by"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    let expected: Vec<String> = (1..=10).map(|i| format!("QP-{i}")).collect();
    assert_eq!(blocked, expected);
}

#[test]
fn tree_with_description_includes_description_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "alpha", "--description", "first task notes"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["tree", "--with-description"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("alpha"));
    assert!(s.contains("first task notes"));
}

#[test]
fn report_json_ticket_emits_parents_children_and_uncapped_events() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "alpha", "--description", "ticket detail body"])
        .assert()
        .success(); // QP-1
    qp(&db)
        .args(["add", "beta", "--depends-on", "QP-1"])
        .assert()
        .success(); // QP-2, depends on QP-1
    qp(&db).args(["add", "gamma"]).assert().success(); // QP-3
    qp(&db)
        .args(["depends", "QP-3", "--on", "QP-1"])
        .assert()
        .success(); // QP-3 also depends on QP-1 -> QP-1's children: QP-2, QP-3
                    // Generate > 10 events on QP-1 so an uncapped event list is meaningfully testable.
    for i in 0..12 {
        qp(&db)
            .args(["log", "QP-1", "note", &format!("note {i}")])
            .assert()
            .success();
    }
    let out = qp(&db)
        .args(["report", "--json", "--ticket", "QP-1"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    assert_eq!(v["display_id"], "QP-1");
    assert_eq!(v["description"], "ticket detail body");
    assert!(v["parents"].as_array().unwrap().is_empty());
    let children: Vec<&str> = v["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["display_id"].as_str().unwrap())
        .collect();
    assert!(children.contains(&"QP-2") && children.contains(&"QP-3"));
    // Uncapped: 12 note events + at least 1 add-time event.
    assert!(v["events"].as_array().unwrap().len() > 10);
}

#[test]
fn report_json_all_tickets_emits_array_of_ticket_detail() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "first"]).assert().success();
    qp(&db).args(["add", "second"]).assert().success();
    let out = qp(&db)
        .args(["report", "--json", "--all-tickets"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    let ids: Vec<&str> = arr
        .iter()
        .map(|t| t["display_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"QP-1") && ids.contains(&"QP-2"));
    assert!(arr[0]["events"].is_array());
}

#[test]
fn list_with_description_includes_description_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "alpha", "--description", "second task notes"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["list", "--with-description"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("alpha"));
    assert!(s.contains("second task notes"));
}

#[test]
fn report_json_emits_tasks_events_deps_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["add", "b", "--depends-on", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["log", "QP-1", "decision", "ok", "--auto"])
        .assert()
        .success();
    let out = qp(&db).args(["report", "--json"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    assert!(v["tasks"].is_array());
    assert!(v["events"].is_array());
    assert!(v["deps"].is_array());
    assert_eq!(v["tasks"].as_array().unwrap().len(), 2);
    // The dep we added
    let deps = v["deps"].as_array().unwrap();
    assert!(deps
        .iter()
        .any(|d| d["from"] == "QP-2" && d["to"] == "QP-1"));
}

#[test]
fn report_json_wave_scope_filters_subtree() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "root"]).assert().success();
    qp(&db).args(["add", "child"]).assert().success();
    qp(&db).args(["add", "unrelated"]).assert().success();
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["report", "--json", "--wave", "QP-1"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    let ids: Vec<&str> = v["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["display_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"QP-1") && ids.contains(&"QP-2"));
    assert!(!ids.contains(&"QP-3"));
}

#[test]
fn depends_rm_emits_state_change_on_promote() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "A"]).assert().success(); // QP-1
    qp(&db)
        .args(["add", "B", "--depends-on", "QP-1"])
        .assert()
        .success(); // QP-2 pending
    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1", "--rm"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["timeline", "QP-2", "--json"])
        .output()
        .unwrap();
    let events: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let arr = events.as_array().expect("timeline json is an array");
    let saw = arr.iter().any(|e| {
        e["kind"] == "state_change"
            && e.get("payload").and_then(|p| p.get("to"))
                == Some(&serde_json::Value::String("ready".into()))
            && e.get("payload").and_then(|p| p.get("via"))
                == Some(&serde_json::Value::String("depends_rm".into()))
    });
    assert!(
        saw,
        "expected state_change to ready via depends_rm in timeline: {:?}",
        arr
    );
}

#[test]
fn list_emits_description_null_when_none() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    let out = qp(&db).args(["list", "--json"]).output().unwrap();
    let arr: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let first = &arr.as_array().unwrap()[0];
    assert!(first.as_object().unwrap().contains_key("description"));
    assert!(first["description"].is_null());
}

#[test]
fn tag_add_emits_event() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["tag", "QP-1", "add", "foo"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["timeline", "QP-1", "--json"])
        .output()
        .unwrap();
    let events: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let arr = events.as_array().unwrap();
    let saw = arr.iter().any(|e| {
        e["kind"] == "tag_added"
            && e.get("payload").and_then(|p| p.get("name"))
                == Some(&serde_json::Value::String("foo".into()))
    });
    assert!(saw, "expected tag_added event: {:?}", arr);
}

#[test]
fn schema_migrates_v1_to_current() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("db.sqlite");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // Minimal v1 shape: meta table + schema_version='1'. No default_tag table.
        conn.execute_batch("
            CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO meta(key, value) VALUES ('schema_version','1');
            INSERT INTO meta(key, value) VALUES ('display_prefix','QP');
            INSERT INTO meta(key, value) VALUES ('project_uuid','00000000-0000-0000-0000-000000000000');
        ").unwrap();
    }
    // qp init on the existing v1 store should migrate it forward.
    qp(&db_path).arg("init").assert().success();
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let v: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, "3", "schema_version should be migrated to 3");
    let has_default_tag: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='default_tag'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        has_default_tag, 1,
        "default_tag table should exist post-migration"
    );
}

#[test]
fn read_command_self_heals_stale_schema_without_init() {
    // The hazard QP-72 flags: a user upgrades the binary onto an old-shape
    // store and runs a READ command first, never `qp init`. `open()` must
    // stay migration-capable or this degrades into `no such table` /
    // `no such column` errors instead of just working.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("db.sqlite");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // Minimal v1 shape: meta table + schema_version='1'. No default_tag table.
        conn.execute_batch("
            CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO meta(key, value) VALUES ('schema_version','1');
            INSERT INTO meta(key, value) VALUES ('display_prefix','QP');
            INSERT INTO meta(key, value) VALUES ('project_uuid','00000000-0000-0000-0000-000000000000');
        ").unwrap();
    }
    // First invocation ever against this store is a read command, not `qp init`.
    qp(&db_path)
        .args(["list", "--json"])
        .assert()
        .success()
        .stdout(contains("[]"));
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let v: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        v, "3",
        "schema_version should self-heal to 3 on a read command"
    );
    let has_default_tag: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='default_tag'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        has_default_tag, 1,
        "default_tag table should exist post-migration"
    );
}

// ─── QP-85 — latest-OPEN-row ownership semantics ───

#[test]
fn depends_uses_latest_open_assignment_not_latest_by_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1
    qp(&db).args(["add", "b"]).assert().success(); // QP-2
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();

    // Engineer divergent state directly: insert a newer CLOSED assignment row
    // on top of the older OPEN one, so latest-by-id and latest-open disagree.
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "INSERT INTO assignment(task_id, agent_id, completed_at, outcome) \
          VALUES (1, 'mallory', '2026-01-01T00:00:00Z', 'success')",
        [],
    )
    .unwrap();
    drop(conn);

    // The stale/closed "latest by id" agent must NOT be treated as owner.
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2", "--as", "mallory"])
        .assert()
        .failure()
        .code(2);

    // The truly-open owner (older row by id, but the only OPEN one) is accepted.
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2", "--as", "alice"])
        .assert()
        .success();
}

#[test]
fn depends_abandon_block_all_reject_wrong_agent_consistently() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();

    // depends
    qp(&db).args(["add", "a"]).assert().success(); // QP-1
    qp(&db).args(["add", "b"]).assert().success(); // QP-2
    qp(&db)
        .args(["assign", "QP-1", "--to", "real"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "real"])
        .assert()
        .success();
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2", "--as", "wrong"])
        .assert()
        .failure()
        .code(2);

    // abandon
    qp(&db).args(["add", "c"]).assert().success(); // QP-3
    qp(&db)
        .args(["assign", "QP-3", "--to", "real"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-3", "--as", "real"])
        .assert()
        .success();
    qp(&db)
        .args(["abandon", "QP-3", "--as", "wrong"])
        .assert()
        .failure()
        .code(2);

    // block
    qp(&db).args(["add", "d"]).assert().success(); // QP-4
    qp(&db)
        .args(["assign", "QP-4", "--to", "real"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-4", "--as", "real"])
        .assert()
        .success();
    qp(&db)
        .args(["block", "QP-4", "--as", "wrong", "--new", "blocker"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn assign_echoes_canonical_id_stripping_whitespace() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    let out = qp(&db)
        .args(["assign", " qp-1 ", "--to", "bob"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_eq!(s, "QP-1 assigned to bob\n");
}

#[test]
fn claim_echoes_canonical_id_for_mixed_case_input() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "bob"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["claim", "qp-1", "--as", "bob"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_eq!(s, "QP-1 claimed by bob\n");
}

#[test]
fn zero_padded_id_resolves_identically_across_show_claim_depends() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1
    qp(&db).args(["add", "b"]).assert().success(); // QP-2

    let out = qp(&db).args(["show", "QP-001"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("QP-1"));

    qp(&db)
        .args(["assign", "QP-001", "--to", "bob"])
        .assert()
        .success();
    let out = qp(&db)
        .args(["claim", "QP-0001", "--as", "bob"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_eq!(s, "QP-1 claimed by bob\n");

    qp(&db)
        .args(["depends", "QP-002", "--on", "QP-01"])
        .assert()
        .success();
}

#[test]
fn legacy_t_form_still_resolves_against_t_prefixed_row() {
    // `T<n>` prefixes predate the 2-5-uppercase-letter `--prefix` validation,
    // so we can't recreate one via `qp init`. Simulate a pre-existing legacy
    // row by writing `display_id` directly, then confirm `T1` still resolves.
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("UPDATE task SET display_id = 'T1' WHERE id = 1", [])
            .unwrap();
    }
    let out = qp(&db).args(["show", "T1"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("T1"));
}

// --- QP-117 / QP-114: --json + error envelope coverage --------------------
//
// Wave 5 shipped `--json` on 12 mutating commands and a 4-variant error
// taxonomy with zero test functions locking either in. These tests parse
// real JSON (never string-match) against the actual `Outcome`/`QuipuError`
// shapes in src/outcome.rs and src/db.rs.

#[test]
fn json_assign_emits_bare_object_with_canonical_display_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    let assert = qp(&db)
        .args(["assign", "QP-1", "--to", "bob", "--json"])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["display_id"], "QP-1");
    assert_eq!(v["agent_id"], "bob");
    assert_eq!(v["state"], "assigned");
    assert!(v.get("ok").is_none(), "must be a bare object, not wrapped");
}

#[test]
fn json_claim_emits_bare_object() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "bob"])
        .assert()
        .success();
    let assert = qp(&db)
        .args(["claim", "QP-1", "--as", "bob", "--json"])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["display_id"], "QP-1");
    assert_eq!(v["agent_id"], "bob");
    assert_eq!(v["state"], "running");
}

#[test]
fn json_complete_emits_bare_object() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "bob"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "bob"])
        .assert()
        .success();
    let assert = qp(&db)
        .args(["complete", "QP-1", "--as", "bob", "--json"])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["display_id"], "QP-1");
    assert_eq!(v["state"], "done");
    assert!(v["decisions"].is_array());
    assert!(v["artifacts"].is_array());
}

#[test]
fn json_cancel_emits_bare_object() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    let assert = qp(&db)
        .args(["cancel", "QP-1", "--reason", "obsolete", "--json"])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["display_id"], "QP-1");
    assert_eq!(v["state"], "cancelled");
    assert_eq!(v["reason"], "obsolete");
}

#[test]
fn json_abandon_emits_bare_object() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    let assert = qp(&db)
        .args(["abandon", "QP-1", "--as", "alice", "--json"])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["display_id"], "QP-1");
    // No unresolved deps -> refresh_ready promotes it straight back to ready.
    assert_eq!(v["state"], "ready");
}

#[test]
fn json_reclaim_emits_bare_object() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    let assert = qp(&db)
        .args(["reclaim", "QP-1", "--reason", "timeout", "--json"])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["display_id"], "QP-1");
    assert_eq!(v["state"], "ready");
    assert_eq!(v["reason"], "timeout");
}

#[test]
fn json_block_exposes_blocker_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    let assert = qp(&db)
        .args([
            "block",
            "QP-1",
            "--as",
            "alice",
            "--new",
            "need infra",
            "--json",
        ])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["display_id"], "QP-1");
    assert_eq!(v["blocker_id"], "QP-2");
    assert_eq!(v["blocker_title"], "need infra");
    assert_eq!(v["state"], "pending");
}

#[test]
fn json_depends_emits_bare_object() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1
    qp(&db).args(["add", "b"]).assert().success(); // QP-2
    let assert = qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1", "--json"])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["display_id"], "QP-2");
    assert_eq!(v["on_id"], "QP-1");
    assert_eq!(v["op"], "add");
}

#[test]
fn json_edit_emits_bare_object() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    let assert = qp(&db)
        .args(["edit", "QP-1", "--title", "new title", "--json"])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["display_id"], "QP-1");
    assert_eq!(v["changed"], true);
    assert!(v["changes"]["title"].is_object());
}

#[test]
fn json_log_emits_bare_object() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    let assert = qp(&db)
        .args([
            "log", "QP-1", "decision", "chose B", "--as", "bob", "--json",
        ])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["display_id"], "QP-1");
    assert_eq!(v["kind"], "decision");
    assert_eq!(v["agent"], "bob");
}

#[test]
fn json_init_emits_bare_object() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let assert = qp(&db).args(["init", "--json"]).assert().success();
    let v = json_stdout(&assert);
    assert!(v["db_path"].is_string());
    assert_eq!(v["prefix"], "QP");
    // `InitOutcome.schema_version` is a `String`, not a number — pinning "3"
    // as a JSON string is intentional, matching the reference output.
    assert_eq!(v["schema_version"], serde_json::Value::String("3".into()));
}

#[test]
fn json_tag_reports_success_in_both_add_and_rm() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();

    // clap quirk: --json must precede the add/rm subcommand.
    let assert = qp(&db)
        .args(["tag", "QP-1", "--json", "add", "kind:x"])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["display_id"], "QP-1");
    assert_eq!(v["op"], "added");
    assert_eq!(v["name"], "kind:x");

    let assert = qp(&db)
        .args(["tag", "QP-1", "--json", "rm", "kind:x"])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["op"], "removed");
    assert_eq!(v["name"], "kind:x");
}

#[test]
fn human_tag_reports_success_in_both_add_and_rm() {
    // Regression guard: `qp tag` used to be silent in human mode. A regression
    // back to silence must fail this test.
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();

    let out = qp(&db)
        .args(["tag", "QP-1", "add", "kind:x"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("QP-1") && s.contains("tagged"), "got: {s:?}");

    let out = qp(&db)
        .args(["tag", "QP-1", "rm", "kind:x"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("QP-1") && s.contains("untagged"), "got: {s:?}");
}

#[test]
fn json_canonicalizes_scruffy_reference_on_assign() {
    // QP-61/QP-78 guarantee, now locked in for the --json path specifically:
    // the reference argument may be scruffy, but the JSON always carries the
    // canonical display_id.
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    let assert = qp(&db)
        .args(["assign", " qp-1 ", "--to", "bob", "--json"])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["display_id"], "QP-1");
}

#[test]
fn json_canonicalizes_zero_padded_reference_on_claim() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "bob"])
        .assert()
        .success();
    let assert = qp(&db)
        .args(["claim", "qp-001", "--as", "bob", "--json"])
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["display_id"], "QP-1");
}

#[test]
fn error_conflict_envelope_on_stderr_for_already_claimed() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "bob"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "bob"])
        .assert()
        .success();
    let assert = qp(&db)
        .args(["claim", "QP-1", "--as", "bob", "--json"])
        .assert()
        .failure()
        .code(2);
    // The envelope must be on stderr, not stdout.
    assert!(
        assert.get_output().stdout.is_empty(),
        "expected empty stdout on error"
    );
    let v = json_stderr(&assert);
    assert_eq!(v["error"]["kind"], "conflict");
    assert_eq!(v["error"]["code"], "already_claimed");
    assert_eq!(v["error"]["task"], "QP-1");
}

#[test]
fn error_not_owner_envelope_on_stderr_for_abandon() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    let assert = qp(&db)
        .args(["abandon", "QP-1", "--as", "bob", "--json"])
        .assert()
        .failure()
        .code(2);
    assert!(
        assert.get_output().stdout.is_empty(),
        "expected empty stdout on error"
    );
    let v = json_stderr(&assert);
    assert_eq!(v["error"]["kind"], "not_owner");
    assert_eq!(v["error"]["message"], "QP-1 not yours");
    assert_eq!(v["error"]["owner"], "alice");
    assert_eq!(v["error"]["task"], "QP-1");
}

#[test]
fn error_not_found_envelope_on_stderr_for_depends_rm() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1
    qp(&db).args(["add", "b"]).assert().success(); // QP-2
    let assert = qp(&db)
        .args(["depends", "QP-2", "--rm", "--on", "QP-1", "--json"])
        .assert()
        .failure()
        .code(2);
    assert!(
        assert.get_output().stdout.is_empty(),
        "expected empty stdout on error"
    );
    let v = json_stderr(&assert);
    assert_eq!(v["error"]["kind"], "not_found");
    assert_eq!(v["error"]["message"], "no dep QP-2 \u{2192} QP-1");
    assert_eq!(v["error"]["task"], "QP-2");
}

#[test]
fn block_wrong_agent_yields_not_owner_not_conflict() {
    // The ownership-vs-state split is the QP-114 deliverable: wrong agent must
    // NOT collapse back into the generic conflict message it used to be.
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "alice"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "alice"])
        .assert()
        .success();
    let assert = qp(&db)
        .args(["block", "QP-1", "--as", "bob", "--new", "x", "--json"])
        .assert()
        .failure()
        .code(2);
    let v = json_stderr(&assert);
    assert_eq!(v["error"]["kind"], "not_owner");
}

#[test]
fn block_wrong_state_yields_conflict_not_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // still `ready`, never assigned
    let assert = qp(&db)
        .args(["block", "QP-1", "--as", "alice", "--new", "x", "--json"])
        .assert()
        .failure()
        .code(2);
    let v = json_stderr(&assert);
    assert_eq!(v["error"]["kind"], "conflict");
    assert_eq!(v["error"]["code"], "not_blockable");
}

#[test]
fn human_mode_renders_prose_not_json_on_success() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    let out = qp(&db)
        .args(["assign", "QP-1", "--to", "bob"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_eq!(s, "QP-1 assigned to bob\n");
    assert!(
        serde_json::from_str::<serde_json::Value>(s.trim()).is_err(),
        "human-mode stdout parsed as JSON, expected prose: {s:?}"
    );
}

#[test]
fn human_mode_renders_prose_not_json_on_error() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "bob"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "bob"])
        .assert()
        .success();
    let assert = qp(&db)
        .args(["claim", "QP-1", "--as", "bob"])
        .assert()
        .failure()
        .code(2);
    let s = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    // Not `starts_with`: warn_on_project_mismatch may emit a `warning:` line first
    // whenever QP_DB differs from the cwd-resolved store, which is every test here.
    // See QP-120.
    assert!(
        s.lines().any(|l| l.starts_with("error: ")),
        "expected an `error: ` line, got: {s:?}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(s.trim()).is_err(),
        "human-mode stderr parsed as JSON, expected prose: {s:?}"
    );
}

// --- QP-117 / QP-115: missing task resolves to NotFound, not internal -----

#[test]
fn show_missing_task_json_is_not_found_and_exits_2() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    let assert = qp(&db)
        .args(["show", "QP-99", "--json"])
        .assert()
        .failure()
        .code(2);
    let v = json_stderr(&assert);
    assert_eq!(v["error"]["kind"], "not_found");
    assert!(
        v["error"].get("code").is_none(),
        "NotFound has no `code` field"
    );
}

#[test]
fn show_missing_task_human_message_stays_readable() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    let assert = qp(&db).args(["show", "QP-99"]).assert().failure().code(2);
    let s = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(s.contains("QP-99"), "expected task ref in message: {s:?}");
    assert!(
        s.contains("no such task"),
        "expected readable message: {s:?}"
    );
}

// --- QP-118: db::State as clap::ValueEnum for --state on list/wait ---------

#[test]
fn list_state_rejects_invalid_spelling_at_parse_time() {
    // Previously `--state redy` silently matched zero rows and exited 0. Now
    // it's a clap parse error: non-zero exit, message naming valid values.
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["list", "--state", "redy"])
        .assert()
        .failure()
        // Exit 1, not 2 (QP-150): a misspelled value is bad input and can never
        // be fixed by retrying; 2 is reserved for store conflicts.
        .code(1)
        .stderr(contains("pending"))
        .stderr(contains("ready"));
}

#[test]
fn wait_state_rejects_invalid_spelling_at_parse_time() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["wait", "--empty", "--state", "bogus"])
        .assert()
        .failure()
        // Exit 1, not 2 — see `list_state_rejects_invalid_spelling_at_parse_time`.
        .code(1)
        .stderr(contains("pending"))
        .stderr(contains("ready"));
}

#[test]
fn list_state_accepts_all_six_valid_spellings() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1, starts ready
    for state in [
        "pending",
        "ready",
        "assigned",
        "running",
        "done",
        "cancelled",
    ] {
        qp(&db).args(["list", "--state", state]).assert().success();
    }
}

#[test]
fn list_state_ready_filters_to_matching_task_only() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1, ready
    qp(&db)
        .args(["add", "b", "--depends-on", "QP-1"])
        .assert()
        .success(); // QP-2, pending (blocked on QP-1)

    let out = qp(&db)
        .args(["list", "--state", "ready", "--json"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["display_id"], "QP-1");
}

#[test]
fn wait_state_pending_blocks_until_task_leaves_pending() {
    // Cohort of one pending task (blocked on an undone dep): --wait --state
    // pending --empty should report non-empty (n=1), so with --timeout-secs 1
    // it hits the timeout exit code (3) rather than returning immediately.
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1, ready
    qp(&db)
        .args(["add", "b", "--depends-on", "QP-1"])
        .assert()
        .success(); // QP-2, pending

    qp(&db)
        .args([
            "wait",
            "--empty",
            "--state",
            "pending",
            "--timeout-secs",
            "1",
        ])
        .assert()
        .failure()
        .code(3);

    // Resolve the dep; the pending cohort drains and --wait returns success.
    qp(&db)
        .args(["cancel", "QP-1", "--reason", "done"])
        .assert()
        .success();
    qp(&db)
        .args([
            "wait",
            "--empty",
            "--state",
            "pending",
            "--timeout-secs",
            "1",
        ])
        .assert()
        .success();
}

#[test]
fn assign_claim_complete_transitions_unchanged_by_state_enum_sweep() {
    // Same transitions as before QP-118's `db::State` param sweep — spelling
    // change only, not semantics. Exercises assign/claim/complete's guarded
    // UPDATEs end to end.
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1, ready

    qp(&db)
        .args(["assign", "QP-1", "--to", "agent-x"])
        .assert()
        .success();
    // Re-assigning an already-assigned task must still fail (guard unchanged).
    qp(&db)
        .args(["assign", "QP-1", "--to", "agent-y"])
        .assert()
        .failure()
        .code(2);

    qp(&db)
        .args(["claim", "QP-1", "--as", "agent-x"])
        .assert()
        .success();
    // Claiming again must still fail — task is no longer `assigned`.
    qp(&db)
        .args(["claim", "QP-1", "--as", "agent-x"])
        .assert()
        .failure()
        .code(2);

    qp(&db)
        .args(["complete", "QP-1", "--as", "agent-x"])
        .assert()
        .success();
    // Completing a done task must still fail — no longer `running`.
    qp(&db)
        .args(["complete", "QP-1", "--as", "agent-x"])
        .assert()
        .failure()
        .code(2);

    let out = qp(&db)
        .args(["list", "--state", "done", "--json"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
}

// --- QP-120: stderr is JSON Lines under --json ----------------------------
//
// `warn_on_project_mismatch` fires only when --db/QP_DB is set explicitly — i.e.
// in automation, which is exactly the consumer that needs stderr parseable. Before
// this, it emitted prose ahead of the JSON error envelope, so `json.loads(stderr)`
// failed and an agent could not read the error it had just been handed.

/// Build two stores with distinct project_uuids, return (cwd_store_dir, other_db).
fn two_stores() -> (tempfile::TempDir, tempfile::TempDir) {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    for d in [&a, &b] {
        Command::cargo_bin("qp")
            .unwrap()
            .current_dir(d.path())
            .arg("init")
            .assert()
            .success();
    }
    (a, b)
}

#[test]
fn json_mode_stderr_is_json_lines_when_project_uuid_mismatches() {
    let (a, b) = two_stores();
    let a_db = a.path().join(".quipu").join("db.sqlite");
    // Seed a failure condition in store A.
    for args in [
        vec!["add", "t"],
        vec!["assign", "QP-1", "--to", "alice"],
        vec!["claim", "QP-1", "--as", "alice"],
    ] {
        Command::cargo_bin("qp")
            .unwrap()
            .env("QP_DB", &a_db)
            .current_dir(a.path())
            .args(&args)
            .assert()
            .success();
    }
    // Now run from store B's directory while pointing QP_DB at store A: the
    // mismatch warning fires, and the double-claim fails.
    let assert = Command::cargo_bin("qp")
        .unwrap()
        .env("QP_DB", &a_db)
        .current_dir(b.path())
        .args(["claim", "QP-1", "--as", "alice", "--json"])
        .assert()
        .failure()
        .code(2);

    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    let lines: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        lines.len() >= 2,
        "expected a warning line and an error line, got: {err:?}"
    );
    // EVERY line must be valid JSON — that is the whole point.
    let parsed: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("stderr line is not JSON: {e}\nline: {l}"))
        })
        .collect();
    assert_eq!(parsed[0]["warning"]["kind"], "project_uuid_mismatch");
    assert!(parsed[0]["warning"]["explicit_uuid"].is_string());
    assert!(parsed[0]["warning"]["cwd_uuid"].is_string());
    // And an agent can still reach the error it actually needs.
    let e = parsed.last().unwrap();
    assert_eq!(e["error"]["kind"], "conflict");
    assert_eq!(e["error"]["code"], "already_claimed");
}

#[test]
fn human_mode_keeps_prose_warning_on_mismatch() {
    let (a, b) = two_stores();
    let a_db = a.path().join(".quipu").join("db.sqlite");
    let assert = Command::cargo_bin("qp")
        .unwrap()
        .env("QP_DB", &a_db)
        .current_dir(b.path())
        .args(["list"])
        .assert()
        .success();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        err.contains("warning: project_uuid mismatch"),
        "human mode should keep the readable warning, got: {err:?}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(err.trim()).is_err(),
        "human mode must not emit JSON, got: {err:?}"
    );
}

/// QP-144: `decisions --since` and `timeline --kind decision --since` must
/// return byte-identical event sets. `decisions` is documented as a filter
/// alias over `timeline`, and both route through one `EventFilter`; this test
/// is what catches a future divergence if someone grows a parallel query.
#[test]
fn decisions_since_matches_timeline_kind_decision_since() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    for i in 0..6 {
        qp(&db)
            .args(["log", "QP-1", "decision", &format!("d{i}"), "--auto"])
            .assert()
            .success();
        // Interleave non-decision events so the two commands must agree on
        // kind filtering as well as on the id bound.
        qp(&db)
            .args(["log", "QP-1", "note", &format!("n{i}")])
            .assert()
            .success();
    }

    let all = json_stdout(&qp(&db).args(["decisions", "--json"]).assert().success());
    let ids: Vec<i64> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids.len(), 6, "expected 6 decisions, got {ids:?}");

    // Probe every boundary, including before-first and past-last.
    let mut probes = vec![0, ids[0] - 1, ids[ids.len() - 1] + 1];
    probes.extend(ids.iter().copied());
    for since in probes {
        let s = since.to_string();
        let d = json_stdout(
            &qp(&db)
                .args(["decisions", "--since", &s, "--json"])
                .assert()
                .success(),
        );
        let t = json_stdout(
            &qp(&db)
                .args(["timeline", "--kind", "decision", "--since", &s, "--json"])
                .assert()
                .success(),
        );
        assert_eq!(d, t, "alias contract broken at --since {since}");
    }
}

/// QP-144: `--since` is an EXCLUSIVE lower bound, matching `timeline`.
/// `--since N` where N is an event id must not return event N itself.
#[test]
fn decisions_since_is_exclusive() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    for i in 0..3 {
        qp(&db)
            .args(["log", "QP-1", "decision", &format!("d{i}")])
            .assert()
            .success();
    }

    let all = json_stdout(&qp(&db).args(["decisions", "--json"]).assert().success());
    let rows = all.as_array().unwrap();
    let first = rows[0]["id"].as_i64().unwrap();

    let after = json_stdout(
        &qp(&db)
            .args(["decisions", "--since", &first.to_string(), "--json"])
            .assert()
            .success(),
    );
    let after_rows = after.as_array().unwrap();
    assert_eq!(
        after_rows.len(),
        rows.len() - 1,
        "--since <first id> must drop exactly the first event"
    );
    assert!(
        after_rows.iter().all(|e| e["id"].as_i64().unwrap() > first),
        "every returned id must be strictly greater than --since"
    );
    assert_eq!(after_rows[0]["payload"]["text"], "d1");
}

/// QP-144: `--since` composes with `--auto-only` — both clauses AND together
/// rather than one silently replacing the other.
#[test]
fn decisions_since_composes_with_auto_only() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    // auto/human pairs, so neither filter alone yields the answer.
    for i in 0..3 {
        qp(&db)
            .args(["log", "QP-1", "decision", &format!("auto{i}"), "--auto"])
            .assert()
            .success();
        qp(&db)
            .args(["log", "QP-1", "decision", &format!("human{i}")])
            .assert()
            .success();
    }

    let all = json_stdout(&qp(&db).args(["decisions", "--json"]).assert().success());
    let rows = all.as_array().unwrap();
    assert_eq!(rows.len(), 6);
    // Cut after the first auto/human pair.
    let cut = rows[1]["id"].as_i64().unwrap();

    let got = json_stdout(
        &qp(&db)
            .args([
                "decisions",
                "--auto-only",
                "--since",
                &cut.to_string(),
                "--json",
            ])
            .assert()
            .success(),
    );
    let texts: Vec<&str> = got
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["payload"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        vec!["auto1", "auto2"],
        "--since must AND with --auto-only, not replace it"
    );
}

// Exit-code contract: broken pipe (QP-139) and argument-parse failures (QP-150).
// ---------------------------------------------------------------------------

/// Runs `qp <args>` with stdout on a pipe, reads a single line, then drops the
/// reader — the `| head -1` idiom, minus the shell.
///
/// Uses `std::process::Command` rather than the `qp()` helper because
/// `assert_cmd` runs the child to completion and hands back a captured buffer;
/// this test is *about* the reader closing early, so it needs the pipe itself.
#[cfg(unix)]
fn run_with_short_reader(db: &std::path::Path, args: &[&str]) -> std::process::ExitStatus {
    use std::io::BufRead;
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("qp"))
        .env("QP_DB", db)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    {
        let stdout = child.stdout.take().unwrap();
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        // `reader` drops here, closing the read end while `qp` is still writing.
    }
    child.wait().unwrap()
}

/// A reader that closes early must never produce exit 101.
///
/// 101 is the Rust panic code: the runtime masks `SIGPIPE`, so `println!` used
/// to panic on `EPIPE` and take a *successful* command's status with it. This is
/// a race between our flushes and the reader's close, not a size threshold, so a
/// single iteration proves nothing — the original bug reproduced 15 times in 20.
/// Hence the loop. Either outcome is correct: exit 0 if every write landed before
/// the close, or death by `SIGPIPE` (signal 13) if it did not.
#[test]
#[cfg(unix)]
fn closed_stdout_pipe_never_exits_101() {
    use std::os::unix::process::ExitStatusExt;

    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    // Enough rows that the writer is still going when the reader leaves.
    for i in 0..60 {
        qp(&db)
            .args([
                "add",
                &format!("task number {i} with a reasonably long title"),
            ])
            .assert()
            .success();
    }
    qp(&db)
        .args(["log", "QP-1", "note", "an event"])
        .assert()
        .success();

    for args in [
        vec!["list"],
        vec!["show", "QP-1"],
        vec!["timeline"],
        vec!["tree"],
        vec!["decisions"],
    ] {
        for iteration in 0..20 {
            let status = run_with_short_reader(&db, &args);
            assert_ne!(
                status.code(),
                Some(101),
                "`qp {}` panicked on a closed pipe (iteration {iteration})",
                args.join(" ")
            );
            let ok = status.success() || status.signal() == Some(libc_sigpipe());
            assert!(
                ok,
                "`qp {}` exited {status:?} on a closed pipe (iteration {iteration}); \
                 expected success or death by SIGPIPE",
                args.join(" ")
            );
        }
    }
}

/// SIGPIPE's number, kept out of the assertion so the intent reads clearly.
#[cfg(unix)]
fn libc_sigpipe() -> i32 {
    13
}

/// An argument typo must be distinguishable from a store conflict.
///
/// Both used to exit 2, the code documented as "conflict — retry may succeed",
/// so a skill retrying on 2 looped forever on a typo. A parse failure is bad
/// input: exit 1, the code `invalid_input` already carries.
#[test]
fn parse_error_and_store_conflict_have_different_exit_codes() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();

    // Genuine conflict: retryable, exit 2.
    let conflict = qp(&db)
        .args(["claim", "QP-1", "--as", "y", "--json"])
        .assert()
        .failure()
        .code(2);
    assert_eq!(json_stderr(&conflict)["error"]["kind"], "conflict");

    // Typo: never retryable, exit 1.
    qp(&db)
        .args(["wait", "--timeout", "1"])
        .assert()
        .failure()
        .code(1);
}

/// A parse failure under `--json` must emit the error envelope, not bare prose.
///
/// Clap exits before `real_main` runs, so this only holds because `main`
/// intercepts the parse error instead of letting the derive default handle it.
/// Without that, stderr carried unparseable prose while claiming to be JSON Lines.
#[test]
fn parse_error_emits_json_envelope() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();

    let assert = qp(&db)
        .args(["wait", "--timeout", "1", "--json"])
        .assert()
        .failure()
        .code(1);
    let env = json_stderr(&assert);
    assert_eq!(env["error"]["kind"], "invalid_input");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--timeout"),
        "envelope should name the offending argument: {env}"
    );

    // An unknown subcommand takes the same path.
    let assert = qp(&db)
        .args(["frobnicate", "--json"])
        .assert()
        .failure()
        .code(1);
    assert_eq!(json_stderr(&assert)["error"]["kind"], "invalid_input");
}

/// `--help` and `--version` are requests for output, not usage errors.
///
/// They arrive as `clap::Error` alongside real parse failures, so the handler
/// has to sort them out; getting this wrong would send help text to exit 1.
#[test]
fn help_and_version_still_exit_zero() {
    Command::cargo_bin("qp")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    Command::cargo_bin("qp")
        .unwrap()
        .arg("--version")
        .assert()
        .success();
}
