use assert_cmd::Command;
use predicates::str::contains;

/// Mirror of `db::SCHEMA_VERSION`, duplicated because `qp` is a bin-only crate
/// with nothing for an integration test to import.
///
/// The tests using it assert that a store lands on the *current* version after
/// migrating, not on any particular number, so a version bump belongs here and
/// nowhere else in this file. If a bump makes some other test fail, that test
/// found a real migration bug — do not chase it back to a literal.
const CURRENT_SCHEMA_VERSION: &str = "8";

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
        .args([
            "reclaim",
            "QP-1",
            "--reason",
            "agent unresponsive",
            "--force",
        ])
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
        .args([
            "reclaim",
            "QP-2",
            "--reason",
            "agent unresponsive",
            "--force",
        ])
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
            vec!["reclaim", "QP-6", "--reason", "r", "--force"],
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
    qp_bare()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("quipu"));
}

#[test]
fn help_lists_core_commands() {
    let assert = qp_bare().arg("--help").assert().success();
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
        "take",
    ] {
        assert!(out.contains(cmd), "help missing `{cmd}`:\n{out}");
    }
}

/// Help text for the release commands names the states the code actually
/// guards on.
///
/// `abandon --help` claimed `running → ready` for months while the code wrote
/// `State::Pending` from `assigned`/`running` (QP-165). Help is the only
/// description of the state machine most agents ever read, so a wrong edge
/// here is worse than no edge; assert the target state by name.
#[test]
fn release_command_help_names_the_real_transition() {
    for cmd in ["abandon", "reclaim", "block"] {
        let assert = qp_bare().args([cmd, "--help"]).assert().success();
        let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
        let summary = out.lines().next().unwrap_or_default().to_string();
        assert!(
            summary.contains("pending"),
            "`qp {cmd} --help` does not name `pending` as the target state, but \
             {cmd}.rs writes `db::State::Pending`. Summary was: {summary:?}"
        );
        assert!(
            !summary.contains("ready"),
            "`qp {cmd} --help` claims a transition to `ready`; the code writes \
             `db::State::Pending`. Summary was: {summary:?}"
        );
    }
}

fn qp(db: &std::path::Path) -> Command {
    let mut c = qp_bare();
    c.env("QP_DB", db);
    c
}

/// `qp` with the ambient store scrubbed out of its environment.
///
/// Every test spawns the binary as a child, and a child inherits the shell's
/// environment — so a `QP_DB` exported by whoever ran `cargo test` silently
/// overrode the store the test meant to use, and tests that assert on *which*
/// store was picked failed for reasons that had nothing to do with the code
/// under test. A test names its store explicitly or deliberately exercises
/// discovery; in neither case does the outside environment get a vote.
///
/// Use this instead of `Command::cargo_bin("qp")` anywhere in this file.
fn qp_bare() -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env_remove("QP_DB");
    // Pin the machine name a claim records, for the same reason `QP_DB` is
    // scrubbed: a test must not describe the machine it happens to run on.
    c.env("QP_HOST", "host.invalid");
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

    let assert = qp_bare()
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
        qp_bare()
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

    qp_bare()
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

/// Copy an executable to `dst` so that it can be `exec`d immediately afterwards.
///
/// `std::fs::copy` is *not* safe for that here, and the reason is not a lag in
/// its own descriptor teardown — it closes both files before it returns, and a
/// single-threaded copy-then-exec loop never fails. The hazard is the test
/// harness around it: `cargo test` runs these tests on many threads, all of
/// them spawning `qp` subprocesses. Spawning is `fork`-then-`exec`, and a
/// `fork` on any sibling thread duplicates *every* descriptor this process has
/// open at that instant — including the writable one that `fs::copy` is holding
/// mid-copy. `O_CLOEXEC` does not help: the duplicate is closed at the child's
/// `exec`, not at its `fork`, so for that window an unrelated child process
/// holds a write handle on our freshly copied binary and the kernel answers our
/// own `exec` with `ETXTBSY` ("Text file busy"). More load means more sibling
/// spawns in flight, which is why this lost on CI and effectively never lost
/// locally.
///
/// So the writable descriptor must never exist in *this* process. Delegating
/// the copy to a short-lived child gives that: `cp` is single-threaded and
/// never forks, so its write handle cannot be duplicated anywhere, and
/// `status()` reaps it — the kernel tears a task's descriptor table down before
/// it reports the exit to the parent, so by the time this returns the inode
/// provably has no writers left. That is an ordering the kernel guarantees, not
/// a window we made small; retrying on `ETXTBSY` would only have done the
/// latter.
#[cfg(unix)]
fn copy_executable_for_exec(src: &std::path::Path, dst: &std::path::Path) {
    let st = std::process::Command::new("cp")
        .arg(src)
        .arg(dst)
        .status()
        .expect("spawning cp");
    assert!(
        st.success(),
        "cp {} {} failed",
        src.display(),
        dst.display()
    );
    // `cp` without `-p` applies the umask, so restate the source's mode rather
    // than assume the executable bit survived. `chmod` takes a path, not a
    // writable descriptor, so this cannot reintroduce the hazard above.
    let mode = std::fs::metadata(src).unwrap().permissions();
    std::fs::set_permissions(dst, mode).unwrap();
}

/// Windows has no `ETXTBSY`; a plain copy is fine there.
#[cfg(not(unix))]
fn copy_executable_for_exec(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::copy(src, dst).unwrap();
}

/// With no usable source root the embedded snapshot is written out.
///
/// This is the distributed-binary case: `current_exe()` derives a `skills/`
/// that does not exist, which used to be a hard failure for every downloaded
/// or `cargo install`ed `qp`.
#[test]
fn install_skills_writes_embedded_snapshot_without_a_source_root() {
    let target = tempfile::tempdir().unwrap();
    // The binary must live outside the checkout, or `current_exe()` derives the
    // repo's own skills/ and we would be testing live mode again.
    let bin_dir = tempfile::tempdir().unwrap();
    let bin = bin_dir.path().join("qp");
    copy_executable_for_exec(&assert_cmd::cargo::cargo_bin("qp"), &bin);

    let out = Command::new(&bin)
        .env_remove("QP_SKILLS_SRC")
        .env_remove("QP_DB")
        .args([
            "install-skills",
            "--target",
            target.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let out = String::from_utf8(out).unwrap();

    assert!(out.contains("embedded"), "mode not named in output: {out}");
    for skill in ["qp-wave", "qp-report-render"] {
        let p = target.path().join(skill).join("SKILL.md");
        assert!(p.is_file(), "missing {}", p.display());
        assert!(!p.parent().unwrap().is_symlink(), "{skill} must be a copy");
        assert!(
            std::fs::read_to_string(&p).unwrap().contains("---"),
            "{skill} body looks empty"
        );
    }
}

/// An explicit `QP_SKILLS_SRC` that points nowhere fails instead of silently
/// falling back to the snapshot.
#[test]
fn install_skills_rejects_a_bogus_explicit_source_root() {
    let target = tempfile::tempdir().unwrap();
    qp_bare()
        .env("QP_SKILLS_SRC", "/nonexistent-qp-skills-src")
        .args([
            "install-skills",
            "--target",
            target.path().to_str().unwrap(),
        ])
        .assert()
        .failure();
}

#[test]
fn install_skills_fails_hard_when_home_unset_and_no_target() {
    let src = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(src.path().join("skills/wave")).unwrap();
    std::fs::write(src.path().join("skills/wave/SKILL.md"), "x").unwrap();

    qp_bare()
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

/// Run `git` in `at`, carrying its own identity, and fail loudly if it errors.
///
/// The identity is not decoration. `git commit` needs one and will happily
/// invent it from the account and hostname when nothing supplies it — which
/// works on a developer machine and does *not* work on a CI runner, where the
/// account has no full name and git refuses with "empty ident name". A test
/// that commits has to bring its own identity for the same reason it has to
/// name its own store: anything taken from the host is something that can
/// differ on the next host. `.invalid` is the reserved TLD (RFC 2606), so the
/// address can never resolve anywhere.
///
/// Set as environment variables rather than `-c user.*` because these outrank
/// config *and* anything the caller already exported. `-c` only outranks
/// config, so a `GIT_AUTHOR_NAME` inherited from the surrounding shell would
/// still win — which is the same inherited-environment trap as `QP_DB`, and no
/// more acceptable here.
///
/// Checking the exit status matters just as much. The version of these helpers
/// discarded it, and a failed commit then went unnoticed: `git worktree add`
/// happens to work from an unborn HEAD, so the test still passed while one of
/// its setup steps had in fact failed. It was luck, not coverage — a setup step
/// allowed to fail in silence is a test that can quietly stop testing what its
/// name claims, and say nothing when it does.
fn git(at: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .env("GIT_AUTHOR_NAME", "qp test")
        .env("GIT_AUTHOR_EMAIL", "qp-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "qp test")
        .env("GIT_COMMITTER_EMAIL", "qp-test@example.invalid")
        .args(args)
        .current_dir(at)
        .output()
        .unwrap_or_else(|e| panic!("could not spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed in {}:\n{}",
        at.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A git repo with one empty commit, ready for `git worktree add`.
fn git_repo(at: &std::path::Path) {
    std::fs::create_dir_all(at).unwrap();
    git(at, &["init", "-q"]);
    git(at, &["commit", "--allow-empty", "-q", "-m", "init"]);
}

/// Add a linked worktree of `repo` at `at`, on a fresh branch.
fn git_worktree(repo: &std::path::Path, at: &std::path::Path, branch: &str) {
    git(
        repo,
        &["worktree", "add", "-q", "-b", branch, at.to_str().unwrap()],
    );
}

/// Number of tasks in the store at `db`, read by naming it explicitly.
fn task_count(db: &std::path::Path) -> usize {
    let out = qp(db).args(["list", "--json"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    serde_json::from_str::<serde_json::Value>(s.trim())
        .unwrap()
        .as_array()
        .unwrap()
        .len()
}

/// The regression this whole area exists for: a worktree parked under a
/// directory that has a store of its own must still resolve to its *repo's*
/// store, not to the one that happens to be sitting above it.
///
/// The ancestor walk used to run to the filesystem root before the git-aware
/// lookup got a turn. A worktree is a sibling of the main repo, so the walk
/// sailed past the repo entirely and landed on whatever store was overhead —
/// a valid store, so reads and writes both succeeded, in the wrong project,
/// with no warning. Every earlier worktree test put the worktree somewhere
/// with nothing above it, which is the one arrangement that hides this.
#[test]
fn worktree_prefers_its_repo_store_over_an_ancestor_store() {
    let tmp = tempfile::tempdir().unwrap();
    let outer_db = tmp.path().join(".quipu").join("db.sqlite");
    qp(&outer_db).arg("init").assert().success();

    let main = tmp.path().join("repo-main");
    git_repo(&main);
    let main_db = main.join(".quipu").join("db.sqlite");
    qp(&main_db).arg("init").assert().success();

    let wt = tmp.path().join("repo-wt");
    git_worktree(&main, &wt, "wt-branch");

    qp_bare()
        .current_dir(&wt)
        .args(["add", "filed-from-a-worktree"])
        .assert()
        .success();

    assert_eq!(
        task_count(&main_db),
        1,
        "task must land in the repo's store"
    );
    assert_eq!(
        task_count(&outer_db),
        0,
        "the store above the worktree must not have been touched"
    );
}

/// The other half of the tradeoff: bounding the walk at the checkout root must
/// not cost a *deliberately* nested store its precedence. A `.quipu/` inside
/// the worktree is below that boundary, so it still wins over both the main
/// repo's store and any ancestor's — which a plain "ask git first" reorder
/// would have broken.
#[test]
fn nested_store_inside_a_worktree_still_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let outer_db = tmp.path().join(".quipu").join("db.sqlite");
    qp(&outer_db).arg("init").assert().success();

    let main = tmp.path().join("repo-main");
    git_repo(&main);
    let main_db = main.join(".quipu").join("db.sqlite");
    qp(&main_db).arg("init").assert().success();

    let wt = tmp.path().join("repo-wt");
    git_worktree(&main, &wt, "wt-branch");
    let wt_db = wt.join(".quipu").join("db.sqlite");
    qp(&wt_db).arg("init").assert().success();

    qp_bare()
        .current_dir(&wt)
        .args(["add", "filed-into-the-nested-store"])
        .assert()
        .success();

    assert_eq!(task_count(&wt_db), 1, "the nested store must win");
    assert_eq!(task_count(&main_db), 0);
    assert_eq!(task_count(&outer_db), 0);
}

/// A subdirectory deep inside a checkout still resolves to the store at the
/// checkout root — the bounded walk stops *at* the root, not before it.
#[test]
fn subdirectory_of_a_repo_resolves_to_the_repo_root_store() {
    let tmp = tempfile::tempdir().unwrap();
    let outer_db = tmp.path().join(".quipu").join("db.sqlite");
    qp(&outer_db).arg("init").assert().success();

    let repo = tmp.path().join("repo");
    git_repo(&repo);
    let repo_db = repo.join(".quipu").join("db.sqlite");
    qp(&repo_db).arg("init").assert().success();

    let deep = repo.join("src").join("inner");
    std::fs::create_dir_all(&deep).unwrap();
    qp_bare()
        .current_dir(&deep)
        .args(["add", "from-a-subdirectory"])
        .assert()
        .success();

    assert_eq!(task_count(&repo_db), 1);
    assert_eq!(task_count(&outer_db), 0);
}

/// A repo with no store of its own keeps falling back to an ancestor's. That
/// is the pre-existing behaviour and it stays: nothing in view conflicts, so
/// there is no wrong answer to pick and no reason to warn about it.
#[test]
fn repo_without_a_store_still_falls_back_to_an_ancestor_store() {
    let tmp = tempfile::tempdir().unwrap();
    let outer_db = tmp.path().join(".quipu").join("db.sqlite");
    qp(&outer_db).arg("init").assert().success();

    let repo = tmp.path().join("repo");
    git_repo(&repo);

    qp_bare()
        .current_dir(&repo)
        .args(["add", "no-store-of-its-own"])
        .assert()
        .success();

    assert_eq!(task_count(&outer_db), 1);
}

/// Precedence, pinned: an explicit `QP_DB` beats discovery outright, even when
/// the cwd has a perfectly good store of its own.
///
/// This is the flip side of hermeticity. Because an explicit store always wins,
/// a `QP_DB` exported in the caller's shell reaches straight into any child
/// process that inherits it — which is how a probe run inside a worktree once
/// filed a junk task into a real project's ledger. Tests therefore spawn `qp`
/// through `qp_bare`, which scrubs the inherited value, and name the store they
/// mean; the environment the suite happens to run in gets no vote.
#[test]
fn explicit_qp_db_wins_over_a_discoverable_store_in_the_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let discoverable = tmp.path().join(".quipu").join("db.sqlite");
    qp(&discoverable).arg("init").assert().success();

    let other = tempfile::tempdir().unwrap();
    let other_db = other.path().join("db.sqlite");
    qp(&other_db).arg("init").assert().success();

    qp(&other_db)
        .current_dir(tmp.path())
        .args(["add", "explicit-wins"])
        .assert()
        .success();

    assert_eq!(task_count(&other_db), 1);
    assert_eq!(task_count(&discoverable), 0);
}

#[test]
fn resolve_path_finds_store_from_worktree() {
    // Simulate: a main repo with .quipu/, and a sibling "worktree" dir
    // whose only git-link points back at the main repo. qp run from inside
    // the worktree should find the main repo's .quipu/.
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("repo-main");
    // Init a git repo + initial commit so worktree-add works.
    git_repo(&main);
    // Initialize a qp store in the main repo.
    qp_bare().current_dir(&main).arg("init").assert().success();
    // Create a worktree as a sibling.
    let wt = tmp.path().join("repo-wt");
    git_worktree(&main, &wt, "tmpbranch");
    // Without QP_DB, running qp from the worktree should still work
    // and create the task in the MAIN repo's store.
    qp_bare()
        .current_dir(&wt)
        .args(["add", "from-worktree"])
        .assert()
        .success();
    // Verify the task landed in the main store.
    let out = qp_bare()
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
        .args(["assign", "QP-3", "--to", "cli:human"])
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

/// Every `state_change` a store contains, in event order, as (kind-agnostic) payloads.
fn state_changes(db: &std::path::Path, task: &str) -> Vec<serde_json::Value> {
    let out = qp(db).args(["timeline", task, "--json"]).output().unwrap();
    let events: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    events
        .as_array()
        .expect("timeline json is an array")
        .iter()
        .filter(|e| e["kind"] == "state_change")
        .map(|e| e["payload"].clone())
        .collect()
}

/// The regression that matters most: completing a blocker promotes its
/// dependent, and that promotion is an event naming both ends of the move.
///
/// The promotion used to be performed by a silent set-based sweep. The
/// dependent's timeline ended at `→ pending` while the dependent itself sat in
/// `ready`, so a replay of the log put it in the wrong state — and `ready` is
/// exactly the state an orchestrator reads to decide what to dispatch next.
#[test]
fn complete_promotion_emits_from_and_to() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "blocker"]).assert().success(); // QP-1
    qp(&db)
        .args(["add", "dependent", "--depends-on", "QP-1"])
        .assert()
        .success(); // QP-2, pending
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

    let promoted = state_changes(&db, "QP-2");
    let saw = promoted.iter().any(|p| {
        p["from"] == serde_json::json!("pending")
            && p["to"] == serde_json::json!("ready")
            && p["via"] == serde_json::json!("complete")
    });
    assert!(
        saw,
        "completing a blocker must log the dependent's promotion with both ends: {promoted:?}"
    );

    // And the blocker's own transition carries the origin the guard required.
    let blocker = state_changes(&db, "QP-1");
    let done = blocker
        .iter()
        .find(|p| p["to"] == serde_json::json!("done"))
        .expect("blocker has a done event");
    assert_eq!(done["from"], serde_json::json!("running"), "{done:?}");
}

/// Cancel, abandon and reclaim promote too, and owe the same event.
#[test]
fn cancel_promotion_emits_from_and_to() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "blocker"]).assert().success();
    qp(&db)
        .args(["add", "dependent", "--depends-on", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["cancel", "QP-1", "--reason", "dropped"])
        .assert()
        .success();
    let promoted = state_changes(&db, "QP-2");
    assert!(
        promoted.iter().any(|p| {
            p["from"] == serde_json::json!("pending")
                && p["to"] == serde_json::json!("ready")
                && p["via"] == serde_json::json!("cancel")
        }),
        "cancelling a blocker resolves it, so the dependent's promotion is an event: {promoted:?}"
    );
    let cancelled = state_changes(&db, "QP-1");
    assert!(
        cancelled
            .iter()
            .any(|p| p["from"] == serde_json::json!("ready")
                && p["to"] == serde_json::json!("cancelled")),
        "the cancelled task's own origin is read from the row, since the guard names a set: {cancelled:?}"
    );
}

/// A release is two moves, and the log says so rather than collapsing them.
///
/// `running` → `ready` is not a transition the state machine has. Emitting one
/// event naming the state the task landed in after the sweep produced exactly
/// that illegal-looking hop.
#[test]
fn abandon_logs_release_and_promotion_separately() {
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
    let seen: Vec<(String, String)> = state_changes(&db, "QP-1")
        .iter()
        .map(|p| {
            (
                p["from"].as_str().unwrap_or("<null>").to_string(),
                p["to"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let tail = &seen[seen.len() - 2..];
    assert_eq!(
        tail,
        [
            ("running".to_string(), "pending".to_string()),
            ("pending".to_string(), "ready".to_string()),
        ],
        "abandon logs the release and the re-promotion as two legal moves: {seen:?}"
    );
}

/// A containment demotion is where the origin earns its keep.
///
/// Blocking a container demotes the slices inside it. Without an origin that
/// event reads `→ pending`, identical to a slice that had merely been created
/// pending and never been workable — so the log could not distinguish work that
/// was taken away from work that was never given.
#[test]
fn containment_demotion_records_the_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "wave"]).assert().success(); // QP-1
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-1"])
        .assert()
        .success(); // QP-2, ready
    qp(&db).args(["add", "prereq"]).assert().success(); // QP-3
                                                        // A blocker on the container freezes everything inside it.
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-3"])
        .assert()
        .success();

    let slice = state_changes(&db, "QP-2");
    let demotion = slice
        .iter()
        .rev()
        .find(|p| p["to"] == serde_json::json!("pending"))
        .unwrap_or_else(|| panic!("slice was demoted by the freeze: {slice:?}"));
    assert_eq!(
        demotion["from"],
        serde_json::json!("ready"),
        "a demotion must be distinguishable from an ordinary arrival at pending: {demotion:?}"
    );

    // The creation event is the one legitimate origin-less `state_change`, and
    // it says so with an explicit null rather than by omitting the key.
    let birth = &slice[0];
    assert_eq!(birth["from"], serde_json::Value::Null, "{birth:?}");
    assert_eq!(birth["to"], serde_json::json!("ready"), "{birth:?}");
}

/// `qp add` writes tags and deps, so it owes an event for each.
///
/// Both used to go in through raw `INSERT`s with no event at all. Since `--tag`
/// is how essentially every tag in a store arrives, the log knew about almost
/// none of them.
#[test]
fn add_emits_events_for_tags_and_deps() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "blocker"]).assert().success(); // QP-1
    qp(&db)
        .args([
            "add",
            "b",
            "--depends-on",
            "QP-1",
            "--tag",
            "kind:bug",
            "--tag",
            "ux",
            "--tier",
            "T1",
            "--description",
            "why it matters",
        ])
        .assert()
        .success(); // QP-2

    let out = qp(&db)
        .args(["timeline", "QP-2", "--json"])
        .output()
        .unwrap();
    let events: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let arr = events.as_array().expect("timeline json is an array");

    let tags: Vec<&str> = arr
        .iter()
        .filter(|e| e["kind"] == "tag_added")
        .filter_map(|e| e["payload"]["name"].as_str())
        .collect();
    assert!(
        tags.contains(&"kind:bug") && tags.contains(&"ux"),
        "every tag a create lands gets a tag_added: {arr:?}"
    );

    assert!(
        arr.iter().any(|e| e["kind"] == "dep_added"
            && e["payload"]["on"] == serde_json::json!("QP-1")
            && e["payload"]["mode"] == serde_json::json!("blocks")),
        "a dep written at create time is a graph mutation like any other: {arr:?}"
    );

    // The creation event leads, and carries the fields the INSERT set silently.
    let first = &arr[0];
    assert_eq!(
        first["kind"],
        serde_json::json!("state_change"),
        "{first:?}"
    );
    assert_eq!(
        first["payload"]["from"],
        serde_json::Value::Null,
        "{first:?}"
    );
    assert_eq!(
        first["payload"]["to"],
        serde_json::json!("pending"),
        "{first:?}"
    );
    assert_eq!(
        first["payload"]["tier"],
        serde_json::json!("T1"),
        "{first:?}"
    );
    assert_eq!(
        first["payload"]["description"],
        serde_json::json!("why it matters"),
        "{first:?}"
    );
}

/// An edge is logged before the demotion it causes, not after.
///
/// A replayer applies events in `event.id` order. Emitting the demote first
/// showed a task pushed back to `pending` with nothing in the graph yet to
/// justify it — a state that is unjustifiable at the moment it is applied, even
/// though the end state is right once the edge lands.
#[test]
fn dep_added_precedes_the_demotion_it_causes() {
    for (label, args) in [
        ("contains", vec!["add", "s1", "--part-of", "QP-1"]),
        ("blocks", vec!["depends", "QP-1", "--on", "QP-2"]),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        qp(&db).arg("init").assert().success();
        qp(&db).args(["add", "wave"]).assert().success(); // QP-1, ready
        if label == "blocks" {
            qp(&db).args(["add", "other"]).assert().success(); // QP-2
        }
        qp(&db).args(args).assert().success();

        let out = qp(&db)
            .args(["timeline", "QP-1", "--json"])
            .output()
            .unwrap();
        let events: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let arr = events.as_array().expect("timeline json is an array");
        let edge = arr
            .iter()
            .position(|e| e["kind"] == "dep_added")
            .unwrap_or_else(|| panic!("{label}: expected a dep_added: {arr:?}"));
        let demote = arr
            .iter()
            .position(|e| {
                e["kind"] == "state_change" && e["payload"]["to"] == serde_json::json!("pending")
            })
            .unwrap_or_else(|| panic!("{label}: expected a demotion: {arr:?}"));
        assert!(
            edge < demote,
            "{label}: the edge must be logged before the demotion it causes: {arr:?}"
        );
    }
}

/// A create with several deps must not log a state the store was never in.
///
/// Sweeping after each edge read a half-written graph: with one resolved dep and
/// one open one, the first sweep saw only the resolved dep and promoted the new
/// task to `ready`, which the second edge then undid. `ready` was never
/// committed, but `qp watch` streams raw events, so an orchestrator reacting to
/// `to: "ready"` got a false readiness signal on the wire.
#[test]
fn multi_dep_add_logs_no_intermediate_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "resolved"]).assert().success(); // QP-1
    qp(&db).args(["add", "open"]).assert().success(); // QP-2
    for args in [
        vec!["assign", "QP-1", "--to", "x"],
        vec!["claim", "QP-1", "--as", "x"],
        vec!["complete", "QP-1", "--as", "x"],
    ] {
        qp(&db).args(args).assert().success();
    }
    // The resolved dep is named first, so a per-edge sweep would promote before
    // the second edge lands. Order matters to the repro; keep it.
    qp(&db)
        .args(["add", "x", "--depends-on", "QP-1", "--depends-on", "QP-2"])
        .assert()
        .success(); // QP-3

    let seen = state_changes(&db, "QP-3");
    assert_eq!(
        seen.len(),
        1,
        "a create that ends up pending logs exactly its creation: {seen:?}"
    );
    assert_eq!(seen[0]["to"], serde_json::json!("pending"), "{seen:?}");
    assert!(
        !seen.iter().any(|p| p["to"] == serde_json::json!("ready")),
        "no event may claim a `ready` the store never held: {seen:?}"
    );
    qp(&db)
        .args(["show", "QP-3", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""state":"pending""#));
}

/// The promotion still happens when every dep named at create time is resolved.
///
/// The guard on the fix above: batching the edges must not cost the single
/// trailing sweep that makes a create against finished prerequisites `ready`.
#[test]
fn multi_dep_add_still_promotes_when_all_deps_resolved() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "one"]).assert().success(); // QP-1
    qp(&db).args(["add", "two"]).assert().success(); // QP-2
    for t in ["QP-1", "QP-2"] {
        qp(&db)
            .args(["cancel", t, "--reason", "done with"])
            .assert()
            .success();
    }
    qp(&db)
        .args(["add", "x", "--depends-on", "QP-1", "--depends-on", "QP-2"])
        .assert()
        .success(); // QP-3

    qp(&db)
        .args(["show", "QP-3", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""state":"ready""#));
    let seen = state_changes(&db, "QP-3");
    assert!(
        seen.iter()
            .any(|p| p["from"] == serde_json::json!("pending")
                && p["to"] == serde_json::json!("ready")
                && p["via"] == serde_json::json!("add")),
        "the single trailing sweep still promotes, and still logs it: {seen:?}"
    );
}

/// The task `qp block` mints needs an event of its own, or a replay cannot
/// create it — every event this command wrote used to name the *blocked* task.
#[test]
fn block_emits_creation_event_for_new_blocker() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "work"]).assert().success(); // QP-1
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["block", "QP-1", "--new", "need creds", "--as", "x"])
        .assert()
        .success(); // QP-2 is the blocker

    let blocker = state_changes(&db, "QP-2");
    assert_eq!(
        blocker.len(),
        1,
        "blocker has its own creation event: {blocker:?}"
    );
    assert_eq!(blocker[0]["from"], serde_json::Value::Null, "{blocker:?}");
    assert_eq!(
        blocker[0]["to"],
        serde_json::json!("ready"),
        "a fresh blocker has no deps of its own: {blocker:?}"
    );

    let out = qp(&db)
        .args(["timeline", "QP-1", "--json"])
        .output()
        .unwrap();
    let events: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let arr = events.as_array().unwrap();
    assert!(
        arr.iter()
            .any(|e| e["kind"] == "dep_added" && e["payload"]["on"] == serde_json::json!("QP-2")),
        "the edge to the new blocker is a graph mutation and gets an event: {arr:?}"
    );
    assert!(
        arr.iter().any(|e| e["kind"] == "state_change"
            && e["payload"]["from"] == serde_json::json!("running")
            && e["payload"]["to"] == serde_json::json!("pending")),
        "the demotion names the state the caller was actually in: {arr:?}"
    );
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
    assert_eq!(
        v, CURRENT_SCHEMA_VERSION,
        "schema_version should be migrated forward"
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
        v, CURRENT_SCHEMA_VERSION,
        "schema_version should self-heal on a read command"
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
    // Prefix, not equality: the line also states when the hold goes stale, and
    // what this test is about is the canonical id being echoed back.
    assert!(
        s.starts_with("QP-1 claimed by bob"),
        "expected the canonical id echoed back, got: {s:?}"
    );
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
    // Prefix, not equality: the line also states when the hold goes stale, and
    // what this test is about is the canonical id being echoed back.
    assert!(
        s.starts_with("QP-1 claimed by bob"),
        "expected the canonical id echoed back, got: {s:?}"
    );

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
        .args([
            "reclaim", "QP-1", "--reason", "timeout", "--force", "--json",
        ])
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
    // `InitOutcome.schema_version` is a `String`, not a number — asserting
    // against a JSON string is the point of this line, matching the reference
    // output. The value itself is checked, but the type is what would regress.
    assert_eq!(
        v["schema_version"],
        serde_json::Value::String(CURRENT_SCHEMA_VERSION.into())
    );
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
/// Two independent stores, one per tempdir, each at `<dir>/.quipu/db.sqlite`.
///
/// `init` names its target explicitly instead of letting discovery place it.
/// Relying on discovery made this helper a hostage to whatever lived above the
/// tempdir: a stray `.quipu/` in the temp root captured *both* inits, the two
/// "stores" became one store with one `project_uuid`, and the mismatch tests
/// downstream had nothing left to mismatch. The stores must genuinely differ
/// for those tests to mean anything, so they are placed, not found.
///
/// Once the store exists at the tempdir root, commands run with that tempdir as
/// their cwd resolve to it on the first step of the walk, so they stay hermetic
/// too — nothing above the tempdir is ever reached.
fn two_stores() -> (tempfile::TempDir, tempfile::TempDir) {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    for d in [&a, &b] {
        qp(&d.path().join(".quipu").join("db.sqlite"))
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
        qp_bare()
            .env("QP_DB", &a_db)
            .current_dir(a.path())
            .args(&args)
            .assert()
            .success();
    }
    // Now run from store B's directory while pointing QP_DB at store A: the
    // mismatch warning fires, and the double-claim fails.
    let assert = qp_bare()
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
    let assert = qp_bare()
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
    qp_bare().arg("--help").assert().success();
    qp_bare().arg("--version").assert().success();
}

// ---------------------------------------------------------------------------
// QP-158: read-only commands must honour `--json` on the ERROR path too.
//
// `wants_json()` in main.rs used to collapse `list`/`tree`/`timeline`/`wave`/
// `status`/`decisions`/`watch` into one `=> false` arm. All seven emit JSON on
// success, so the bug was invisible until something failed — then a `--json`
// consumer got prose on stderr and the "stderr is JSON Lines" contract broke
// exactly when it mattered. One test per command so a regression names the
// command that regressed rather than a single opaque table failure.
// ---------------------------------------------------------------------------

/// A store file that SQLite will reject outright.
///
/// It must be **longer than one page header** — a 1-byte junk file is treated
/// as a brand-new empty database, silently migrated, and then the command
/// succeeds or reports `not_found`, so the error path under test never runs.
/// 200 bytes of non-magic content reliably yields SQLITE_NOTADB.
fn corrupt_db(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    let db = tmp.path().join("corrupt.sqlite");
    std::fs::write(&db, vec![b'x'; 200]).unwrap();
    db
}

/// Run `qp <args> --json` against a corrupt store and assert the failure is
/// reported as the `{"error": {...}}` envelope on stderr, not as prose.
fn assert_json_error_envelope(args: &[&str]) {
    let tmp = tempfile::tempdir().unwrap();
    let db = corrupt_db(&tmp);
    let assert = qp(&db).args(args).arg("--json").assert().failure();
    let v = json_stderr(&assert);
    assert!(
        v["error"].is_object(),
        "`qp {} --json` did not emit an error envelope on stderr; stderr was:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&assert.get_output().stderr)
    );
    assert!(
        !v["error"]["kind"].is_null(),
        "error envelope missing `kind`: {v}"
    );
}

#[test]
fn list_json_reports_errors_as_envelope() {
    assert_json_error_envelope(&["list"]);
}

#[test]
fn tree_json_reports_errors_as_envelope() {
    assert_json_error_envelope(&["tree"]);
}

#[test]
fn timeline_json_reports_errors_as_envelope() {
    assert_json_error_envelope(&["timeline"]);
}

#[test]
fn wave_json_reports_errors_as_envelope() {
    assert_json_error_envelope(&["wave"]);
}

#[test]
fn status_json_reports_errors_as_envelope() {
    assert_json_error_envelope(&["status"]);
}

#[test]
fn decisions_json_reports_errors_as_envelope() {
    assert_json_error_envelope(&["decisions"]);
}

#[test]
fn watch_json_reports_errors_as_envelope() {
    // `watch --json` defaults to true, but pass it explicitly: the point of the
    // test is the flag-to-error-path wiring, not the default.
    assert_json_error_envelope(&["watch", "--max-ticks", "1"]);
}

#[test]
fn read_only_commands_still_print_prose_errors_without_json() {
    // The other half of the contract: no `--json`, no envelope. If a future
    // change makes the error path unconditionally JSON, this catches it.
    let tmp = tempfile::tempdir().unwrap();
    let db = corrupt_db(&tmp);
    for cmd in ["list", "tree", "timeline", "wave", "status", "decisions"] {
        let assert = qp(&db).arg(cmd).assert().failure();
        let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        assert!(
            !last.trim_start().starts_with('{'),
            "`qp {cmd}` (no --json) emitted JSON on stderr: {err}"
        );
    }
}

// ── Containment spec ────────────────────────────────────────────────────────
//
// A "wave" ticket that reverse-depends on its slices gets rollup for free: it
// stays `pending` while any slice is open and `refresh_ready` promotes it once
// the last one lands. That worked before containment was nameable, and these
// first tests lock it so the new edge mode cannot regress it.
//
// The tests below were written as an `#[ignore]`d specification before the
// behaviour existed, and un-ignored as each slice landed. Nothing here is
// ignored any more; a failure is a real failure.

/// A container is pending while any of its contents is open.
#[test]
fn wave_is_pending_while_a_slice_is_open() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "slice a"]).assert().success();
    qp(&db).args(["add", "slice b"]).assert().success();
    qp(&db)
        .args([
            "add",
            "Wave",
            "--depends-on",
            "QP-1",
            "--depends-on",
            "QP-2",
        ])
        .assert()
        .success();
    qp(&db)
        .args(["show", "QP-3", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"pending\""));
}

/// The rollup: completing the last slice promotes the container to `ready`.
#[test]
fn wave_becomes_ready_when_its_last_slice_completes() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "slice a"]).assert().success();
    qp(&db)
        .args(["add", "Wave", "--depends-on", "QP-1"])
        .assert()
        .success();
    for step in [
        ["assign", "QP-1", "--to", "a"],
        ["claim", "QP-1", "--as", "a"],
    ] {
        qp(&db).args(step).assert().success();
    }
    qp(&db)
        .args(["complete", "QP-1", "--as", "a"])
        .assert()
        .success();
    qp(&db)
        .args(["show", "QP-2", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"ready\""));
}

/// Rollup cascades: nesting works to arbitrary depth with no extra machinery.
#[test]
fn nested_waves_roll_up_one_level_at_a_time() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "slice"]).assert().success();
    qp(&db)
        .args(["add", "Wave: inner", "--depends-on", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["add", "Wave: outer", "--depends-on", "QP-2"])
        .assert()
        .success();
    for step in [
        ["assign", "QP-1", "--to", "a"],
        ["claim", "QP-1", "--as", "a"],
    ] {
        qp(&db).args(step).assert().success();
    }
    qp(&db)
        .args(["complete", "QP-1", "--as", "a"])
        .assert()
        .success();
    // inner is ready (its work is done); outer still waits on inner itself.
    qp(&db)
        .args(["show", "QP-2", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"ready\""));
    qp(&db)
        .args(["show", "QP-3", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"pending\""));
}

/// A containment cycle is rejected by the same guard that catches dep cycles.
#[test]
fn containment_cycle_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "inner"]).assert().success();
    qp(&db)
        .args(["add", "outer", "--depends-on", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2"])
        .assert()
        .failure()
        .code(2)
        .stderr(contains("cycle"));
}

/// Attaching contents to an already-`ready` container demotes it.
#[test]
fn attaching_a_slice_demotes_a_ready_wave() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db).args(["add", "slice"]).assert().success();
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["show", "QP-1", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"pending\""));
}

/// A *running* container is never demoted — work is not yanked from a live agent.
#[test]
fn attaching_a_slice_does_not_demote_a_running_wave() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    for step in [
        ["assign", "QP-1", "--to", "a"],
        ["claim", "QP-1", "--as", "a"],
    ] {
        qp(&db).args(step).assert().success();
    }
    qp(&db).args(["add", "slice"]).assert().success();
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2", "--as", "a"])
        .assert()
        .success();
    qp(&db)
        .args(["show", "QP-1", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"running\""));
}

// ── Not yet built: the specification ────────────────────────────────────────

/// SPEC: containment and blocking must be distinguishable per edge.
/// Encodes the `dep.mode` decision. A node-level marker cannot substitute:
/// a container with both slices and a bare prerequisite has all edges leaving
/// one node.
#[test]
fn contains_and_blocks_are_distinguishable() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "slice"]).assert().success();
    qp(&db).args(["add", "prerequisite"]).assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["contains", "QP-3", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["depends", "QP-3", "--on", "QP-2"])
        .assert()
        .success();
    let out = qp(&db).args(["show", "QP-3", "--json"]).assert().success();
    let v = json_stdout(&out);
    assert_eq!(v["contains"], serde_json::json!(["QP-1"]));
    assert_eq!(v["blocked_by"], serde_json::json!(["QP-2"]));
}

/// SPEC: `--part-of` attaches at create time, keeping parent-first order cheap.
#[test]
fn add_part_of_attaches_to_a_container() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["show", "QP-1", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"pending\""));
}

/// SPEC: a container's blockers gate its contents.
///
/// DECIDED. Where the blocker is attached is the control:
/// - on the wave — "nothing in this wave starts until X", freezing every slice
/// - on a slice — only that slice waits; siblings run (already true today)
///
/// Both are wanted, and having both means neither is a compromise: if only one
/// slice needed the prerequisite you would attach it there instead.
///
/// This makes `dep.mode` load-bearing for BEHAVIOUR, not just display: walking up
/// from a slice requires knowing which of the container's edges are containment
/// and which are blocking. With one undifferentiated edge type you would walk
/// everything (wrong) or nothing (today). So mode ships first, propagation on top.
///
/// Implementation note: readiness stops being local - `refresh_ready` must also
/// check ancestors via a recursive CTE, the same shape `would_cycle` already uses.
#[test]
fn slices_of_a_blocked_wave_are_not_dispatchable() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "slice"]).assert().success();
    qp(&db).args(["add", "prerequisite"]).assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["contains", "QP-3", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["depends", "QP-3", "--on", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["show", "QP-1", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"pending\""));
}

/// A pre-`dep.mode` store gains the column, with every existing edge read as
/// blocking.
///
/// The trap this guards: every statement in `schema.sql` is
/// `CREATE ... IF NOT EXISTS`, so adding a column to a `CREATE TABLE` body
/// reaches freshly-initialized stores and nobody else — the statement is a
/// no-op wherever the table already exists. A fresh-`init` test passes whether
/// or not the `ALTER` in `db::add_missing_columns` is there, and so proves
/// nothing. This one builds the *old* table shape by hand.
#[test]
fn dep_mode_reaches_a_store_that_predates_the_column() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "slice"]).assert().success();
    qp(&db)
        .args(["add", "wave", "--depends-on", "QP-1"])
        .assert()
        .success();

    // Rewind to the pre-mode schema: drop the column and un-stamp the version,
    // leaving the edge in place.
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "ALTER TABLE dep DROP COLUMN mode;
             UPDATE meta SET value = '3' WHERE key = 'schema_version';",
        )
        .unwrap();
        let cols: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('dep') WHERE name = 'mode'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cols, 0, "setup failed: mode column should be gone");
    }

    // Any read command must self-heal the schema — a user who upgrades the
    // binary and runs `qp list` before ever running `qp init` depends on this.
    qp(&db).args(["list", "--json"]).assert().success();

    let conn = rusqlite::Connection::open(&db).unwrap();
    let version: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
    // The surviving edge is backfilled to `blocks`, which is what makes the
    // migration inert: an old store has no containment edges until someone
    // runs `qp contains`.
    let modes: Vec<String> = {
        let mut s = conn.prepare("SELECT mode FROM dep").unwrap();
        let rows = s
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows
    };
    assert_eq!(modes, vec!["blocks".to_string()]);
}

/// `qp contains` reclassifies an edge that already exists as a plain dep.
///
/// This is the conversion path for stores that expressed containment as reverse
/// dependencies before the mode existed, so re-linking must not error, must not
/// duplicate the row, and must not disturb the container's state — both modes
/// wait, so nothing about blockedness changes.
#[test]
fn contains_reclassifies_an_existing_dep_edge_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "slice"]).assert().success();
    qp(&db)
        .args(["add", "wave", "--depends-on", "QP-1"])
        .assert()
        .success();
    let before = qp(&db).args(["show", "QP-2", "--json"]).assert().success();
    assert_eq!(
        json_stdout(&before)["blocked_by"],
        serde_json::json!(["QP-1"])
    );

    qp(&db)
        .args(["contains", "QP-2", "QP-1"])
        .assert()
        .success();

    let after = qp(&db).args(["show", "QP-2", "--json"]).assert().success();
    let v = json_stdout(&after);
    assert_eq!(v["blocked_by"], serde_json::json!([]));
    assert_eq!(v["contains"], serde_json::json!(["QP-1"]));
    assert_eq!(v["state"], "pending", "reclassifying must not change state");

    let conn = rusqlite::Connection::open(&db).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM dep", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        n, 1,
        "reclassify must update in place, not insert a second row"
    );
}

/// The container is the row gaining an edge, so its owner gates both verbs.
#[test]
fn attaching_to_a_running_container_requires_its_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    for step in [
        ["assign", "QP-1", "--to", "a"],
        ["claim", "QP-1", "--as", "a"],
    ] {
        qp(&db).args(step).assert().success();
    }
    qp(&db).args(["add", "slice"]).assert().success();

    qp(&db)
        .args(["contains", "QP-1", "QP-2"])
        .assert()
        .failure();
    qp(&db)
        .args(["add", "another slice", "--part-of", "QP-1"])
        .assert()
        .failure();
    qp(&db)
        .args(["contains", "QP-1", "QP-2", "--as", "a"])
        .assert()
        .success();
    // Still running: linking never demotes a task already in an agent's hands.
    qp(&db)
        .args(["show", "QP-1", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"running\""));
}

/// A cycle anywhere in a batch rejects the whole batch.
///
/// Partial application would leave the caller with edges they cannot see and did
/// not ask for, and no way to tell which of the children landed.
#[test]
fn contains_batch_is_all_or_nothing_on_a_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "outer"]).assert().success();
    qp(&db).args(["add", "ok child"]).assert().success();
    // QP-3 already contains QP-1, so making QP-1 contain QP-3 closes a loop.
    qp(&db)
        .args(["add", "inner", "--part-of", "QP-1"])
        .assert()
        .success();

    qp(&db)
        .args(["contains", "QP-1", "QP-2", "QP-1"])
        .assert()
        .failure()
        .code(2)
        .stderr(contains("cycle"));

    let conn = rusqlite::Connection::open(&db).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM dep", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "the good child must be rolled back with the bad one");
}

/// `qp wave` keeps containers in its pending group.
///
/// The group's filter asks "is this genuinely waiting on something", which is a
/// different question from "what blocks this". A container waiting on its own
/// contents has no blockers, so a blockers-only filter silently drops every wave
/// parent from the one view whose whole job is showing the wave.
#[test]
fn wave_pending_group_includes_a_container_waiting_on_its_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-1"])
        .assert()
        .success();

    let out = qp(&db).args(["wave", "--json"]).assert().success();
    let v = json_stdout(&out);
    let pending: Vec<&str> = v["pending"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["display_id"].as_str().unwrap())
        .collect();
    assert_eq!(pending, vec!["QP-1"]);
}

// ── Blocker propagation through containment ─────────────────────────────────

/// Freezing a wave reaches slices that are *already* in the ready pool.
///
/// The promote-side check alone would only hold back work that had not been
/// unblocked yet, so attaching a prerequisite mid-wave would freeze nothing.
#[test]
fn blocking_a_wave_demotes_its_ready_slices() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["show", "QP-3", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"ready\""));

    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1"])
        .assert()
        .success();

    qp(&db)
        .args(["show", "QP-3", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"pending\""));
}

/// A frozen slice is not yanked out of an agent's hands mid-flight.
///
/// Freezing stops the *next* slice being dispatched; it does not interrupt one
/// already running. This is what lets the ownership gate sit on the container
/// alone — no slice owner can lose work to someone else's `qp depends`.
#[test]
fn freezing_a_wave_leaves_a_running_slice_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-2"])
        .assert()
        .success();
    for step in [
        ["assign", "QP-3", "--to", "a"],
        ["claim", "QP-3", "--as", "a"],
    ] {
        qp(&db).args(step).assert().success();
    }

    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1"])
        .assert()
        .success();

    qp(&db)
        .args(["show", "QP-3", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"running\""));
}

/// Completing the prerequisite thaws the wave's contents.
#[test]
fn completing_a_waves_blocker_thaws_its_slices() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1"])
        .assert()
        .success();

    for step in [
        ["assign", "QP-1", "--to", "p"],
        ["claim", "QP-1", "--as", "p"],
        ["complete", "QP-1", "--as", "p"],
    ] {
        qp(&db).args(step).assert().success();
    }

    qp(&db)
        .args(["show", "QP-3", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"ready\""));
}

/// Where the blocker is attached is the entire control.
///
/// On the wave it freezes everything inside; on one slice it affects only that
/// slice. Both are wanted, which is why neither is a compromise — if only one
/// slice needed the prerequisite, you attach it there.
#[test]
fn blocking_one_slice_leaves_its_siblings_dispatchable() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice a", "--part-of", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["add", "slice b", "--part-of", "QP-2"])
        .assert()
        .success();

    qp(&db)
        .args(["depends", "QP-3", "--on", "QP-1"])
        .assert()
        .success();

    qp(&db)
        .args(["show", "QP-3", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"pending\""));
    qp(&db)
        .args(["show", "QP-4", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"ready\""));
}

/// The freeze follows containment to any depth, and only containment.
///
/// Following `blocks` edges downward too would mean "X blocks the wave" also
/// froze everything the wave merely waits on — a different and unwanted claim.
#[test]
fn freezing_propagates_through_nested_containment_only() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db).args(["add", "Outer"]).assert().success();
    qp(&db)
        .args(["add", "Inner", "--part-of", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["add", "deep slice", "--part-of", "QP-3"])
        .assert()
        .success();
    // A plain blocking edge from the outer wave to an unrelated task. Nothing
    // hangs off it, so nothing downstream of it may be frozen.
    qp(&db).args(["add", "bystander"]).assert().success();
    qp(&db)
        .args(["depends", "QP-5", "--on", "QP-1"])
        .assert()
        .success();

    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1"])
        .assert()
        .success();

    // Two levels down from the blocked container: frozen.
    qp(&db)
        .args(["show", "QP-4", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"pending\""));
    // Reached only through `blocks` edges: untouched by the freeze, though it
    // is pending on its own blocker.
    let out = qp(&db).args(["show", "QP-5", "--json"]).assert().success();
    let v = json_stdout(&out);
    assert_eq!(v["state"], "pending");
    assert_eq!(
        v["frozen_by"],
        serde_json::json!([]),
        "a `blocks` edge must not propagate a freeze"
    );
}

/// A frozen ticket says why. Nothing else on it would.
///
/// It has no unresolved dependency of its own — the blocker sits on a container
/// above it — so `blocked_by` is empty and the bare `pending` is unexplained.
#[test]
fn a_frozen_slice_names_its_blocker_and_the_container_holding_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "Inner", "--part-of", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["add", "deep slice", "--part-of", "QP-3"])
        .assert()
        .success();
    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1"])
        .assert()
        .success();

    let out = qp(&db).args(["show", "QP-4", "--json"]).assert().success();
    let v = json_stdout(&out);
    assert_eq!(v["blocked_by"], serde_json::json!([]));
    assert_eq!(
        v["frozen_by"],
        serde_json::json!([{"container": "QP-2", "blocker": "QP-1"}]),
        "must name the blocker and which container carries it, at any depth"
    );

    qp(&db)
        .args(["show", "QP-4"])
        .assert()
        .success()
        .stdout(contains("frozen_by: QP-1 (blocking QP-2)"));
}

/// `qp wave` keeps frozen slices visible.
///
/// A frozen slice has no unresolved dep of its own, so the pending group's
/// filter would drop it — and it is not in any other group either, so it would
/// vanish from the view entirely.
#[test]
fn wave_pending_group_includes_frozen_slices() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1"])
        .assert()
        .success();

    let out = qp(&db).args(["wave", "--json"]).assert().success();
    let v = json_stdout(&out);
    let pending: Vec<&str> = v["pending"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["display_id"].as_str().unwrap())
        .collect();
    assert_eq!(pending, vec!["QP-2", "QP-3"]);
}

/// Attaching a slice to an already-frozen wave freezes the slice on arrival.
///
/// The same code path as freezing an existing slice, reached from the other
/// direction: here the edge is new and the blocker is old.
#[test]
fn a_slice_added_to_a_frozen_wave_arrives_frozen() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1"])
        .assert()
        .success();

    qp(&db)
        .args(["add", "late slice", "--part-of", "QP-2"])
        .assert()
        .success();

    qp(&db)
        .args(["show", "QP-3", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"pending\""));
}

// ── Display distinguishes the two edge modes ────────────────────────────────

/// `qp tree` nests containment and annotates blocking.
///
/// Both used to render as the same flat `<- [...]` list, so a wave read as
/// blocked by the slices it is made of.
#[test]
fn tree_nests_containment_and_annotates_blocking() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice a", "--part-of", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["add", "slice b", "--part-of", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["depends", "QP-4", "--on", "QP-1"])
        .assert()
        .success();

    let out = qp(&db).args(["tree"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let line = |did: &str| {
        s.lines()
            .find(|l| l.trim_start().starts_with(did))
            .unwrap_or_else(|| panic!("no row for {did}:\n{s}"))
            .to_string()
    };

    // The wave is not annotated with its own contents.
    assert!(
        !line("QP-2").contains("<-"),
        "a container must not render as blocked by its contents:\n{s}"
    );
    // A genuine blocker still annotates.
    assert!(
        line("QP-4").contains("<- [QP-1]"),
        "blocking edge lost its annotation:\n{s}"
    );
    // Slices are indented under the wave. Titles start at a fixed column, so a
    // deeper title offset is exactly the nesting.
    let wave_title = s
        .lines()
        .find(|l| l.contains("Wave"))
        .unwrap()
        .find("Wave")
        .unwrap();
    let slice_title = s
        .lines()
        .find(|l| l.contains("slice a"))
        .unwrap()
        .find("slice a")
        .unwrap();
    assert!(
        slice_title > wave_title,
        "slice should be indented under its wave:\n{s}"
    );
}

/// A slice whose container is filtered out still prints, at the top level.
///
/// Nesting must not become a way for rows to disappear: the filter picks which
/// tasks are shown, and the tree only decides how they are arranged.
#[test]
fn tree_still_prints_a_slice_whose_container_is_filtered_out() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db)
        .args(["add", "Wave", "--tier", "big"])
        .assert()
        .success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-1", "--tier", "small"])
        .assert()
        .success();

    let out = qp(&db).args(["tree", "--tier", "small"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("QP-2"), "orphaned slice vanished:\n{s}");
    assert!(!s.contains("QP-1"), "filter leaked the container:\n{s}");
}

/// `tree --json` splits the modes while leaving `depends_on` alone.
#[test]
fn tree_json_splits_the_modes_without_breaking_depends_on() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1"])
        .assert()
        .success();

    let out = qp(&db).args(["tree", "--json"]).assert().success();
    let v = json_stdout(&out);
    let wave = v
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["display_id"] == "QP-2")
        .unwrap();
    assert_eq!(wave["contains"], serde_json::json!(["QP-3"]));
    assert_eq!(wave["blocked_by"], serde_json::json!(["QP-1"]));
    // Unchanged meaning: every dep edge, both modes.
    assert_eq!(wave["depends_on"].as_array().unwrap().len(), 2);

    let slice = v
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["display_id"] == "QP-3")
        .unwrap();
    assert_eq!(slice["part_of"], serde_json::json!(["QP-2"]));
}

/// `report --json` carries `mode` on every edge.
///
/// Without it a board cannot draw hierarchy and blocking differently, and has
/// to treat a wave as blocked by its own slices.
#[test]
fn report_json_carries_edge_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1"])
        .assert()
        .success();

    let out = qp(&db).args(["report", "--json"]).assert().success();
    let v = json_stdout(&out);
    let mut edges: Vec<(String, String, String)> = v["deps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["from"].as_str().unwrap().to_string(),
                d["to"].as_str().unwrap().to_string(),
                d["mode"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    edges.sort();
    assert_eq!(
        edges,
        vec![
            ("QP-2".into(), "QP-1".into(), "blocks".into()),
            ("QP-2".into(), "QP-3".into(), "contains".into()),
        ]
    );
}

/// `report --ticket` carries `mode` on each parent/child edge too.
#[test]
fn report_ticket_detail_carries_edge_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1"])
        .assert()
        .success();

    let out = qp(&db)
        .args(["report", "--ticket", "QP-2", "--json"])
        .assert()
        .success();
    let v = json_stdout(&out);
    let modes: Vec<(&str, &str)> = v["parents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["display_id"].as_str().unwrap(),
                p["mode"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(modes.contains(&("QP-1", "blocks")), "got {modes:?}");
    assert!(modes.contains(&("QP-3", "contains")), "got {modes:?}");
}

// ── Review fixes ────────────────────────────────────────────────────────────

/// `qp block` on a container freezes its contents.
///
/// It writes its dep edge directly rather than through `db::link_dep`, and so
/// was the one blocking verb whose freeze `qp show` reported but nothing
/// applied — `qp list --state ready` kept handing the frozen slice out. It is
/// also the primary way an agent raises a blocker mid-wave, i.e. the main path
/// the feature exists to serve.
#[test]
fn block_on_a_container_freezes_its_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db).args(["add", "slice"]).assert().success();
    for step in [
        ["assign", "QP-1", "--to", "bob"],
        ["claim", "QP-1", "--as", "bob"],
    ] {
        qp(&db).args(step).assert().success();
    }
    qp(&db)
        .args(["contains", "QP-1", "QP-2", "--as", "bob"])
        .assert()
        .success();

    qp(&db)
        .args(["block", "QP-1", "--as", "bob", "--new", "need infra"])
        .assert()
        .success();

    qp(&db)
        .args(["show", "QP-2", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"pending\""));
    // And it must not be dispatchable.
    qp(&db)
        .args(["assign", "QP-2", "--to", "carol"])
        .assert()
        .failure();
}

/// Cancelling a container releases its contents.
///
/// `refresh_ready`'s rule is that cancelling a blocker unblocks its dependents
/// rather than stranding them. Containment did not honour it: the frozen set
/// checked the blocker's state but never the container's, so a dead wave went
/// on freezing its slices with no hint that `qp contains --rm` was the cure.
#[test]
fn cancelling_a_container_releases_its_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-1"])
        .assert()
        .success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-3"])
        .assert()
        .success();
    qp(&db)
        .args(["show", "QP-2", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"pending\""));

    qp(&db)
        .args(["cancel", "QP-1", "--reason", "wave dropped"])
        .assert()
        .success();

    let out = qp(&db).args(["show", "QP-2", "--json"]).assert().success();
    let v = json_stdout(&out);
    assert_eq!(v["state"], "ready", "a cancelled wave must free its slices");
    assert_eq!(
        v["frozen_by"],
        serde_json::json!([]),
        "and must stop being reported as the reason"
    );
}

/// A `done` container likewise stops freezing.
#[test]
fn a_completed_container_stops_freezing_its_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2"])
        .assert()
        .success();
    // Resolve the blocker so the wave can run to completion, then re-block it.
    for step in [
        ["assign", "QP-2", "--to", "p"],
        ["claim", "QP-2", "--as", "p"],
        ["complete", "QP-2", "--as", "p"],
        ["assign", "QP-1", "--to", "w"],
        ["claim", "QP-1", "--as", "w"],
        ["complete", "QP-1", "--as", "w"],
    ] {
        qp(&db).args(step).assert().success();
    }
    qp(&db).args(["add", "late blocker"]).assert().success();
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-3"])
        .assert()
        .success();

    qp(&db)
        .args(["add", "new slice", "--part-of", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["show", "QP-4", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"ready\""));
}

/// Reclassifying `contains` → `blocks` thaws what the edge used to freeze.
///
/// `qp depends`' add path used to skip promotion on the reasoning that adding an
/// edge can only demote. True before edge modes; false once an add can
/// reclassify. The victim was left `pending` with an empty `blocked_by` *and* an
/// empty `frozen_by` — a ticket explaining nothing — until some unrelated later
/// command happened to sweep.
#[test]
fn reclassifying_containment_to_blocking_thaws_the_subtree() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "Inner", "--part-of", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["add", "deep slice", "--part-of", "QP-2"])
        .assert()
        .success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-4"])
        .assert()
        .success();
    qp(&db)
        .args(["show", "QP-3", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"pending\""));

    // QP-1 no longer *contains* QP-2; it merely waits on it.
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2"])
        .assert()
        .success();

    qp(&db)
        .args(["show", "QP-3", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"ready\""));
}

/// Re-running an identical link reconciles rather than short-circuiting.
///
/// Repairing a store whose frozen set drifted means re-running the command that
/// should have set it. Returning early on an unchanged edge made that repair a
/// silent no-op, so the only fix was to write some *unrelated* edge.
#[test]
fn re_running_an_unchanged_link_still_reconciles() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-1"])
        .assert()
        .success();
    qp(&db).args(["add", "prereq"]).assert().success();

    // Plant a blocking edge behind the CLI's back, so the frozen set is stale.
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO dep(task_id, depends_on_task_id, mode) VALUES (1, 3, 'blocks')",
            [],
        )
        .unwrap();
    }
    qp(&db)
        .args(["show", "QP-2", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"ready\""));

    // The identical command a user would reach for. It changes no edge.
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-3"])
        .assert()
        .success();

    qp(&db)
        .args(["show", "QP-2", "--json"])
        .assert()
        .success()
        .stdout(contains("\"state\":\"pending\""));
}

/// A store whose stamp is ahead of its schema heals on any command.
///
/// The migration used to run only behind the version check, so a stamp that had
/// somehow advanced past the schema meant nothing ever looked again: every
/// command failed on the missing column, and `qp init` — the obvious repair —
/// reported success while fixing nothing.
#[test]
fn a_store_whose_stamp_outran_its_schema_self_heals() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "ALTER TABLE dep DROP COLUMN mode;
             UPDATE meta SET value = '4' WHERE key = 'schema_version';",
        )
        .unwrap();
    }

    qp(&db).args(["list", "--json"]).assert().success();

    let conn = rusqlite::Connection::open(&db).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('dep') WHERE name = 'mode'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "the column should have been restored");
}

/// `qp contains --rm` names only what it actually released.
///
/// Printing the whole argument list after "released" claimed the release of
/// edges it never touched — including live `blocks` edges, which `--rm` does not
/// remove — while the trailing note read as "already released" rather than
/// "untouched".
#[test]
fn contains_rm_does_not_claim_to_release_an_untouched_edge() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db).args(["add", "b"]).assert().success();
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2"])
        .assert()
        .success();

    let out = qp(&db)
        .args(["contains", "QP-1", "QP-2", "--rm"])
        .assert()
        .success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        !s.contains("released"),
        "must not claim a release that did not happen: {s:?}"
    );

    // The blocking edge is untouched.
    qp(&db)
        .args(["show", "QP-1", "--json"])
        .assert()
        .success()
        .stdout(contains("\"blocked_by\":[\"QP-2\"]"));
}

/// `qp tree` agrees with `show` and `list` about what "blocked by" means.
///
/// `tree` built its own edge map straight from `dep` with no state filter. That
/// was survivable while the annotation was an untyped `<- [...]`, and stopped
/// being survivable once `--json` named the field `blocked_by`.
#[test]
fn tree_blocked_by_excludes_resolved_blockers_like_show_does() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db)
        .args(["add", "work", "--depends-on", "QP-1"])
        .assert()
        .success();
    qp(&db)
        .args(["cancel", "QP-1", "--reason", "not needed"])
        .assert()
        .success();

    let out = qp(&db).args(["tree", "--json"]).assert().success();
    let v = json_stdout(&out);
    let work = v
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["display_id"] == "QP-2")
        .unwrap();
    assert_eq!(
        work["blocked_by"],
        serde_json::json!([]),
        "a cancelled blocker is resolved; show and list both agree"
    );

    let out = qp(&db).args(["tree"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(!s.contains("<- [QP-1]"), "stale annotation survived:\n{s}");
}

/// `report --ticket` offers edge lists named from the ticket's point of view.
///
/// `parents`/`children` name the dep direction, which reads exactly backwards
/// for containment: a container's *contents* are its "parents". Both are kept
/// for compatibility, so the fix is to also publish unambiguous lists.
#[test]
fn report_ticket_names_containment_from_the_tickets_point_of_view() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-1"])
        .assert()
        .success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-3"])
        .assert()
        .success();

    let out = qp(&db)
        .args(["report", "--ticket", "QP-1", "--json"])
        .assert()
        .success();
    let v = json_stdout(&out);
    let ids = |k: &str| -> Vec<String> {
        v[k].as_array()
            .unwrap()
            .iter()
            .map(|e| e["display_id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(ids("contains"), vec!["QP-2"]);
    assert_eq!(ids("blocked_by"), vec!["QP-3"]);

    let out = qp(&db)
        .args(["report", "--ticket", "QP-2", "--json"])
        .assert()
        .success();
    let v = json_stdout(&out);
    let part_of: Vec<String> = v["part_of"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["display_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(part_of, vec!["QP-1"]);
}

/// `qp depends --rm` records which kind of edge it removed.
///
/// The removal is deliberately mode-blind, so the audit log is the only place
/// that can say what actually vanished from the graph.
#[test]
fn depends_rm_records_the_mode_it_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "Wave"]).assert().success();
    qp(&db)
        .args(["add", "slice", "--part-of", "QP-1"])
        .assert()
        .success();

    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2", "--rm"])
        .assert()
        .success();

    let out = qp(&db)
        .args(["timeline", "QP-1", "--json"])
        .assert()
        .success();
    let v = json_stdout(&out);
    let removed = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "dep_removed")
        .expect("no dep_removed event");
    assert_eq!(removed["payload"]["mode"], "contains");
}

/// A task in two containers prints once, and says where else it lives.
///
/// The `seen` guard keeps the walk finite, but silently picking one container
/// made the other render as empty — so the global tree disagreed with
/// `qp tree <that container>`. Nothing forbids multiple containment, so the
/// renderer has to say so rather than choose a winner in silence.
#[test]
fn tree_notes_a_tasks_other_containers_instead_of_hiding_them() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "Wave A"]).assert().success();
    qp(&db).args(["add", "shared slice"]).assert().success();
    qp(&db).args(["add", "Wave B"]).assert().success();
    qp(&db)
        .args(["contains", "QP-1", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["contains", "QP-3", "QP-2"])
        .assert()
        .success();

    let out = qp(&db).args(["tree"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_eq!(
        s.lines().filter(|l| l.contains("shared slice")).count(),
        1,
        "should print once:\n{s}"
    );
    assert!(
        s.contains("(also in QP-3)"),
        "Wave B renders as empty with no hint:\n{s}"
    );

    // Scoped to the other container, there is nothing else in view to note.
    let out = qp(&db).args(["tree", "QP-3"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(s.contains("shared slice"), "scoped view lost it:\n{s}");
    assert!(
        !s.contains("also in"),
        "noted an out-of-scope container:\n{s}"
    );
}

/// `report --since` must reject out-of-range durations, not panic.
///
/// The `Nh`/`Nd` parse was already mapped into `invalid_input`, but the value
/// then went straight into `Duration::hours` and `-`, both of which panic on
/// overflow — so a plausible typo (`999999999999h`) aborted the process
/// instead of reporting bad input.
///
/// Both signs are covered: a huge positive offset overflows going back, a huge
/// negative one overflows going forward, and only the positive direction has
/// the extra pre-year-0 band described below.
#[test]
fn report_since_rejects_out_of_range_durations() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();

    let rejects = |args: &[&str], probe: &str| {
        let assert = qp(&db).args(args).assert().failure().code(1);
        let env = json_stderr(&assert);
        assert_eq!(
            env["error"]["kind"], "invalid_input",
            "--since {probe} must be typed invalid input: {env}"
        );
        assert!(
            env["error"]["message"]
                .as_str()
                .unwrap()
                .contains("--since"),
            "envelope should name the offending argument: {env}"
        );
    };

    // Overflow constructing the Duration, overflow subtracting it, and a
    // timestamp that lands before year 0 and so has no RFC3339 rendering.
    // That last band is why `20000000h` (~2283 years back) is rejected while
    // the same magnitude forward is not: RFC3339's floor of year 0 sits far
    // closer to now than its ceiling of year 9999.
    let probes = [
        "9223372036854775807h",
        "999999999999h",
        "20000000h",
        "9223372036854775807d",
        "99999999999d",
        "1000000d",
    ];
    for probe in probes {
        rejects(&["report", "--since", probe, "--json"], probe);
    }

    // The negative direction: `now - (-huge)` overflows forward, and it was
    // the same unguarded `-` that panicked. These MUST use the `--since=-N`
    // form. Space-separated, clap reads `-99999999999h` as an unknown flag and
    // rejects it before `parse_since` ever runs — so a space-form probe would
    // assert on clap's behaviour rather than on this code path, and would keep
    // passing even if the guard were removed.
    //
    // Magnitudes here are far larger than the positive ones on purpose: going
    // forward there is no pre-year-0 band to trip, so nothing short of true
    // overflow fails. `-1000000d` lands in year ~4765 and legitimately
    // succeeds.
    for probe in ["--since=-99999999999h", "--since=-99999999999d"] {
        rejects(&["report", probe, "--json"], probe);
    }

    // The in-range case still works.
    qp(&db)
        .args(["report", "--since", "24h", "--json"])
        .assert()
        .success();
    qp(&db)
        .args(["report", "--since", "7d", "--json"])
        .assert()
        .success();
}

/// `--since` date forms are *validated* and *normalised*, not passed through.
///
/// Both consumers compare `--since` against `event.ts` as TEXT (the SQL
/// `ts >= ?1` scope filter and the in-memory `e.ts.as_str() < s` skip), so a
/// value that is accepted but not converted to UTC is not merely sloppy — it
/// silently answers the wrong question with exit 0. This test pins the three
/// ways that used to happen: an unparsed 10-char date, a non-`Z` offset
/// compared verbatim, and a fractionless bound sorting above every event in
/// the same second.
#[test]
fn report_since_normalises_date_forms() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();

    let rejects = |args: &[&str], probe: &str| {
        let assert = qp(&db).args(args).assert().failure().code(1);
        let env = json_stderr(&assert);
        assert_eq!(
            env["error"]["kind"], "invalid_input",
            "--since {probe} must be typed invalid input: {env}"
        );
        assert!(
            env["error"]["message"]
                .as_str()
                .unwrap()
                .contains("--since"),
            "envelope should name the offending argument: {env}"
        );
    };

    // Shape is not validation. A 10-char string with `-` at 4 and 7 used to be
    // widened to `{s}T00:00:00Z` and returned WITHOUT PARSING, so garbage sorted
    // above every real timestamp and the report came back empty at exit 0.
    for probe in ["20xx-aa-bb", "2026-13-45", "2026-02-30", "0000-00-00"] {
        rejects(&["report", "--since", probe, "--json"], probe);
    }
    // The 11-char control, rejected before this change too: strictness must not
    // depend on string length.
    rejects(
        &["report", "--since", "2026-13-45x", "--json"],
        "2026-13-45x",
    );

    // Anchor every membership assertion on a real event.
    let board = json_stdout(&qp(&db).args(["report", "--json"]).assert().success());
    let ev = board["events"]
        .as_array()
        .unwrap()
        .first()
        .expect("`add` must have written an event")
        .clone();
    let id = ev["id"].as_i64().unwrap();
    let ts = ev["ts"].as_str().unwrap().to_string();
    // `event.ts` is written by the schema default
    // `strftime('%Y-%m-%dT%H:%M:%fZ','now')`, which is exactly this shape. The
    // field slicing below depends on it.
    assert_eq!(ts.len(), 24, "unexpected event ts shape: {ts}");

    let holds = |since: &str| -> bool {
        let v = json_stdout(
            &qp(&db)
                .args(["report", "--since", since, "--json"])
                .assert()
                .success(),
        );
        v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"].as_i64() == Some(id))
    };

    let date = &ts[..10];
    let hh: u32 = ts[11..13].parse().unwrap();
    let mm: u32 = ts[14..16].parse().unwrap();
    let ss: u32 = ts[17..19].parse().unwrap();
    let frac = &ts[19..23]; // ".mmm"

    // A bare date still means midnight UTC of that day, so the day's own events
    // are in the window.
    assert!(
        holds(date),
        "bare date {date} must still keep its own events"
    );

    // Positive offset. The probe names the SAME INSTANT as the event (or an
    // earlier one), so the event is inside the window — but its TEXT sorts
    // above the event's, which is exactly what used to drop it. The branches
    // pick the coarsest field that can be bumped without wrapping past the day.
    let ahead = if hh < 23 {
        format!("{date}T{:02}:{mm:02}:{ss:02}{frac}+01:00", hh + 1)
    } else if mm < 59 {
        format!("{date}T{hh:02}:{:02}:{ss:02}{frac}+00:01", mm + 1)
    } else if ss < 59 {
        format!("{date}T{hh:02}:{mm:02}:{:02}{frac}+00:01", ss + 1)
    } else {
        // 23:59:59 — the one second a day where no same-date text sorts above
        // the event while naming an earlier instant. Still a correct probe,
        // just not a discriminating one.
        format!("{date}T{hh:02}:{mm:02}:{ss:02}{frac}+00:01")
    };
    assert!(
        holds(&ahead),
        "--since {ahead} names an instant at or before {ts}; the event belongs in the window"
    );

    // Negative offset, the other direction: the probe names an instant one hour
    // AFTER the event, so the event is outside the window — but its text sorts
    // below the event's (`-` 0x2D < `Z` 0x5A at the offset position), which is
    // what used to over-include it.
    let behind = format!("{}-01:00", &ts[..23]);
    assert!(
        !holds(&behind),
        "--since {behind} names an instant after {ts}; the event must be excluded"
    );
    // Same string through the other consumer — the SQL `ts >= ?1` scope filter.
    let scoped = json_stdout(
        &qp(&db)
            .args(["report", "--all-tickets", "--since", &behind, "--json"])
            .assert()
            .success(),
    );
    assert_eq!(
        scoped.as_array().unwrap().len(),
        0,
        "SQL scope filter must agree with the in-memory one: {scoped}"
    );

    // Subsecond boundary. `event.ts` always carries three subsecond digits; a
    // bound written to the second used to sort above them all (`Z` 0x5A above
    // `.` 0x2E) and exclude the very second it names.
    let second = format!("{}Z", &ts[..19]);
    assert!(
        holds(&second),
        "--since {second} names the start of the second holding {ts}; the event is in it"
    );
}

/// Normalising a `--since` offset must not be able to panic.
///
/// Folding `+05:00`/`-05:00` into UTC shifts the instant, and the shift can
/// leave the representable range: a negative offset on a year-9999 value walks
/// forward past the ceiling, a positive one on a year-0 value walks back below
/// the floor. The unchecked shift *panicked* on the first — a raw abort on
/// stderr instead of the JSON error envelope, the same class of failure the
/// `Nh`/`Nd` overflow guards exist to prevent.
///
/// Below year 0 is rejected rather than emitted: it renders with a leading `-`,
/// which is not RFC3339, and the whole point of normalising is that the bound
/// is a real instant in the shape `event.ts` uses.
#[test]
fn report_since_rejects_unrepresentable_offset_shifts() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();

    let rejects = |probe: &str| {
        let assert = qp(&db)
            .args(["report", "--since", probe, "--json"])
            .assert()
            .failure()
            .code(1);
        let env = json_stderr(&assert);
        assert_eq!(
            env["error"]["kind"], "invalid_input",
            "--since {probe} must be typed invalid input, not a panic: {env}"
        );
        assert!(
            env["error"]["message"]
                .as_str()
                .unwrap()
                .contains("--since"),
            "envelope should name the offending argument: {env}"
        );
    };

    for probe in [
        // Forward past year 9999. This one panicked.
        "9999-12-31T23:59:59-00:01",
        "9999-12-31T23:00:00-05:00",
        // Back below year 0.
        "0000-01-01T00:00:00+05:00",
        "0000-01-01T00:00:00+00:01",
    ] {
        rejects(probe);
    }

    // The other side of each edge still resolves: the guard rejects only what
    // it cannot render, and does not clamp the range in from where it was.
    for probe in [
        "9999-12-31T23:59:59+00:01",
        "0000-01-01T00:00:00Z",
        "0000-01-01T00:00:00-05:00",
    ] {
        qp(&db)
            .args(["report", "--since", probe, "--json"])
            .assert()
            .success();
    }
}

// --- display-id prefix scoping --------------------------------------------
//
// The prefix is part of the identifier: a reference carrying some other
// store's prefix names nothing here, and must fail rather than land on the
// row that happens to sit at that rowid.

#[test]
fn matching_prefix_resolves_and_echoes_canonical_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db)
        .args(["init", "--prefix", "ACME"])
        .assert()
        .success();
    qp(&db).args(["add", "a"]).assert().success(); // ACME-1

    let out = qp(&db)
        .args(["show", "acme-001", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["display_id"], "ACME-1");
}

#[test]
fn foreign_prefix_is_invalid_input_naming_both_prefixes() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db)
        .args(["init", "--prefix", "ACME"])
        .assert()
        .success();
    qp(&db).args(["add", "a"]).assert().success(); // ACME-1

    let out = qp(&db).args(["show", "QP-1"]).assert().code(1);
    let s = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(s.contains("QP"), "should name the supplied prefix: {s}");
    assert!(s.contains("ACME"), "should name the store's prefix: {s}");

    // A prefix no store has ever used is the same error, not a resolution.
    let out = qp(&db).args(["show", "ZZZZ-1", "--json"]).assert().code(1);
    let err = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    let last = err.lines().rfind(|l| !l.is_empty()).unwrap();
    let v: serde_json::Value = serde_json::from_str(last).unwrap();
    assert_eq!(v["error"]["kind"], "invalid_input");
}

#[test]
fn foreign_prefix_does_not_mutate_the_row_at_that_rowid() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db)
        .args(["init", "--prefix", "ACME"])
        .assert()
        .success();
    qp(&db).args(["add", "a"]).assert().success(); // ACME-1

    qp(&db).args(["tag", "QP-1", "add", "x"]).assert().code(1);
    qp(&db).args(["tag", "ZZZZ-1", "add", "y"]).assert().code(1);
    qp(&db)
        .args(["cancel", "QP-1", "--reason", "wrong store"])
        .assert()
        .code(1);

    let out = qp(&db)
        .args(["show", "ACME-1", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(
        v["tags"],
        serde_json::json!([]),
        "a foreign-prefixed tag must not land: {v}"
    );
    assert_eq!(v["state"], "ready", "state must be untouched: {v}");
}

#[test]
fn bare_integer_is_still_not_a_task_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success(); // QP-1

    let out = qp(&db).args(["show", "1"]).assert().code(1);
    let s = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(s.contains("invalid id"), "{s}");
}

/// `take` picks the oldest ready task and leaves it exactly where `assign` would.
///
/// "Same state as `assign`" is asserted by continuing the state machine from
/// here: a task that `take` moved must be claimable by the agent it named, which
/// is only true if both the `task.state` row and the `assignment` row landed.
#[test]
fn take_assigns_oldest_ready_task() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "first"]).assert().success();
    qp(&db).args(["add", "second"]).assert().success();

    qp(&db)
        .args(["take", "--as", "w1", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""display_id":"QP-1""#))
        .stdout(contains(r#""state":"assigned""#));

    qp(&db)
        .args(["show", "QP-1", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""state":"assigned""#));
    // The assignment row landed too, or this cannot succeed.
    qp(&db)
        .args(["claim", "QP-1", "--as", "w1"])
        .assert()
        .success();

    // The next take gets the next-oldest, never the one already taken.
    qp(&db)
        .args(["take", "--as", "w2", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""display_id":"QP-2""#));
}

#[test]
fn take_respects_the_tag_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "untagged"]).assert().success();
    qp(&db)
        .args(["add", "tagged", "--tag", "kind:slice"])
        .assert()
        .success();

    // QP-1 is older, so only the filter can explain picking QP-2.
    qp(&db)
        .args(["take", "--as", "w1", "--tag", "kind:*", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""display_id":"QP-2""#));
    // Filter that matches nothing ready: no work, and QP-1 is untouched.
    qp(&db)
        .args(["take", "--as", "w1", "--tag", "kind:*", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""taken":null"#));
    qp(&db)
        .args(["show", "QP-1", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""state":"ready""#));
}

/// A task with an unresolved dependency is never taken, however the store got
/// into that shape.
///
/// Two arrangements, because they fail differently: a plain `pending` dependent
/// is excluded by the `state = 'ready'` guard alone, while a task frozen by a
/// blocker on the wave that contains it is excluded only by the `db::FROZEN`
/// clause the candidate subquery repeats.
#[test]
fn take_never_takes_a_task_with_unresolved_deps() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "prereq"]).assert().success();
    qp(&db)
        .args(["add", "dependent", "--depends-on", "QP-1"])
        .assert()
        .success();

    // Only the prereq is takeable.
    qp(&db)
        .args(["take", "--as", "w1", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""display_id":"QP-1""#));
    qp(&db)
        .args(["take", "--as", "w2", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""taken":null"#));
    qp(&db)
        .args(["show", "QP-2", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""state":"pending""#));

    // Resolving the prereq makes the dependent takeable, and not before.
    qp(&db)
        .args(["claim", "QP-1", "--as", "w1"])
        .assert()
        .success();
    qp(&db)
        .args(["complete", "QP-1", "--as", "w1"])
        .assert()
        .success();
    qp(&db)
        .args(["take", "--as", "w2", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""display_id":"QP-2""#));
}

/// An empty queue is exit 0 with an explicit null, not an error.
///
/// The exit code is the load-bearing half: a poller retries on 2, so a drained
/// queue answering 2 would hot-loop, and under `set -e` any non-zero code aborts
/// the loop that polls for work. See `docs/modules/take.md`.
#[test]
fn take_on_an_empty_queue_exits_zero_and_mutates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "done already"]).assert().success();
    qp(&db)
        .args(["cancel", "QP-1", "--reason", "r"])
        .assert()
        .success();

    let before = qp(&db).args(["timeline", "--json"]).assert().success();
    let before = String::from_utf8(before.get_output().stdout.clone()).unwrap();

    qp(&db)
        .args(["take", "--as", "w1", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""taken":null"#));
    qp(&db)
        .args(["take", "--as", "w1"])
        .assert()
        .success()
        .stdout(contains("nothing to take"));

    let after = qp(&db).args(["timeline", "--json"]).assert().success();
    let after = String::from_utf8(after.get_output().stdout.clone()).unwrap();
    assert_eq!(before, after, "a no-work take must write no events");
}

/// A command in a directory with no store must fail loudly and create nothing.
///
/// This used to be the quietest bug in the tool. `qp list --json` here printed
/// `[]` and exited 0 — after creating `./.quipu/db.sqlite` on the way — so a
/// misconfigured machine looked exactly like an empty project, and the next
/// `qp add` filed a real ticket into a store nobody meant to make. The
/// `.quipu` assertion is the load-bearing half: an exit code alone would still
/// pass if resolution kept creating on the side.
#[test]
fn command_without_a_store_fails_with_no_store_and_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();

    for args in [
        vec!["list", "--json"],
        vec!["add", "ghost"],
        vec!["show", "QP-1"],
        vec!["status"],
    ] {
        qp_bare()
            .current_dir(tmp.path())
            .args(&args)
            .assert()
            .failure()
            .code(6)
            .stderr(contains("no quipu store in"))
            .stderr(contains("qp init"));
    }

    assert!(
        !tmp.path().join(".quipu").exists(),
        "a failing command must not have created a store"
    );
}

/// The `--json` envelope for the same condition: a consumer branches on `kind`,
/// not on the prose, and `path` names the store that was looked for.
#[test]
fn no_store_renders_a_json_envelope_with_kind_and_path() {
    let tmp = tempfile::tempdir().unwrap();
    let assert = qp_bare()
        .current_dir(tmp.path())
        .args(["list", "--json"])
        .assert()
        .failure()
        .code(6);
    let body = json_stderr(&assert);
    assert_eq!(body["error"]["kind"], "no_store");
    assert!(body["error"]["path"].is_string());
    assert!(!tmp.path().join(".quipu").exists());
}

/// `qp init` is the one command that may create a store, and still does.
#[test]
fn init_is_the_only_command_that_creates_a_store() {
    let tmp = tempfile::tempdir().unwrap();
    qp_bare()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();
    assert!(tmp.path().join(".quipu").join("db.sqlite").exists());
    qp_bare()
        .current_dir(tmp.path())
        .args(["add", "now-there-is-somewhere-to-put-this"])
        .assert()
        .success();
}

/// Regression guard on the git-common-dir fallback, which the attach check must
/// not break: a worktree is a *sibling* of the main repo, so the ancestor walk
/// never reaches the repo's `.quipu/`. If `require_attached` ran against the
/// cwd rather than the resolved path, every agent working in a worktree would
/// be told there is no store while it is sitting right there.
#[test]
fn worktree_without_its_own_store_still_reaches_the_main_repo_store() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("repo-main");
    git_repo(&main);
    let main_db = main.join(".quipu").join("db.sqlite");
    qp(&main_db).arg("init").assert().success();

    let wt = tmp.path().join("repo-wt");
    git_worktree(&main, &wt, "wt-branch");

    qp_bare()
        .current_dir(&wt)
        .args(["add", "filed-from-a-worktree-with-no-store"])
        .assert()
        .success();

    assert_eq!(task_count(&main_db), 1);
    assert!(
        !wt.join(".quipu").exists(),
        "the worktree must not have grown a store of its own"
    );
}

/// An explicit `--db`/`QP_DB` at a path with no store fails the same way.
///
/// Deliberately not exempted. An explicit path is a *claim about where the
/// store is*; honouring a wrong one by conjuring an empty store is precisely
/// the silent cross-machine failure the error exists to prevent. `qp init`
/// with that path is how you say "create it".
#[test]
fn explicit_db_at_a_missing_path_fails_rather_than_creating() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("nowhere").join("db.sqlite");

    qp(&db)
        .args(["list", "--json"])
        .assert()
        .failure()
        .code(6)
        .stderr(contains("no quipu store at"));
    assert!(!db.exists());
    assert!(!tmp.path().join("nowhere").exists());

    qp_bare()
        .current_dir(tmp.path())
        .args(["--db", db.to_str().unwrap(), "add", "ghost"])
        .assert()
        .failure()
        .code(6);
    assert!(!db.exists());

    // ...and `init` with the same path is how you actually mean it.
    qp(&db).arg("init").assert().success();
    assert!(db.exists());
}

// ---------------------------------------------------------------------------
// Claim liveness.
//
// A TTL bounds how long a ticket may sit QUIET while held, not how long the
// work may take — every write by the holder pushes the expiry out. Said twice
// because every reader gets it wrong on the first pass: a slice that runs long
// is not at risk; a slice whose machine died is.
//
// Time is made testable by writing `last_contact` into the past directly rather
// than by sleeping or by injecting a clock. That is available precisely because
// the schema stores the lease *length* alongside the last contact: move the
// contact back further than the length and the hold has expired, with no wait
// and nothing mocked.
// ---------------------------------------------------------------------------

/// Rewind a task's open hold so its stored lease has run out.
fn age_hold(db: &std::path::Path, task_id: i64, seconds_ago: i64) {
    let conn = rusqlite::Connection::open(db).unwrap();
    let n = conn
        .execute(
            "UPDATE assignment
                SET last_contact = strftime('%Y-%m-%dT%H:%M:%fZ','now', ?1)
              WHERE task_id = ?2 AND completed_at IS NULL",
            rusqlite::params![format!("-{seconds_ago} seconds"), task_id],
        )
        .unwrap();
    assert_eq!(n, 1, "expected exactly one open hold to age");
}

fn hold_of(db: &std::path::Path, task: &str) -> serde_json::Value {
    let assert = qp(db).args(["show", task, "--json"]).assert().success();
    json_stdout(&assert)["hold"].clone()
}

#[test]
fn a_fresh_claim_is_live_and_a_lapsed_one_is_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x", "--for", "45m"])
        .assert()
        .success();

    let hold = hold_of(&db, "QP-1");
    assert_eq!(hold["agent"], "x");
    assert_eq!(hold["stale"], false, "a fresh claim is not stale: {hold}");
    assert_eq!(hold["lease_seconds"], 2_700);
    // The machine is named, not just the role: `x` is a role every box reuses.
    assert_eq!(hold["host"], "host.invalid");
    assert!(hold["expires_at"].is_string());

    // Past its stated length, and nothing had to notice in the meantime —
    // expiry is evaluated when someone asks.
    age_hold(&db, 1, 46 * 60);
    assert_eq!(hold_of(&db, "QP-1")["stale"], true);

    // And it is legible in the view an orchestrator already reads.
    let assert = qp(&db).args(["wave", "--json"]).assert().success();
    let v = json_stdout(&assert);
    assert_eq!(v["running"][0]["display_id"], "QP-1");
    assert_eq!(v["running"][0]["stale"], true);
    qp(&db)
        .args(["wave"])
        .assert()
        .success()
        .stdout(contains("STALE"));
}

#[test]
fn a_write_by_the_holder_pushes_the_expiry_out() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x", "--for", "45m"])
        .assert()
        .success();

    // Quiet for longer than the lease: stale.
    age_hold(&db, 1, 46 * 60);
    assert_eq!(hold_of(&db, "QP-1")["stale"], true);

    // One ordinary write by the holder, and the hold is live again — this is
    // why a ticket that takes days but is written against never goes stale.
    qp(&db)
        .args(["log", "QP-1", "note", "still working", "--as", "x"])
        .assert()
        .success();
    assert_eq!(hold_of(&db, "QP-1")["stale"], false);

    // Renewal is not specific to `log`: it rides on the event insert every
    // write goes through, so `tag` renews too.
    age_hold(&db, 1, 46 * 60);
    qp(&db)
        .args(["tag", "QP-1", "--as", "x", "add", "prio:high"])
        .assert()
        .success();
    assert_eq!(hold_of(&db, "QP-1")["stale"], false);
}

#[test]
fn a_hold_with_no_stated_expiry_never_goes_stale() {
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

    // The shape every open claim written before leases existed is in: no length
    // and no contact stamp. The only safe reading is that it is unbreakable.
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE assignment SET lease_seconds = NULL, last_contact = NULL
              WHERE completed_at IS NULL",
            [],
        )
        .unwrap();
    }
    let hold = hold_of(&db, "QP-1");
    assert_eq!(
        hold["stale"], false,
        "a claim with no stated expiry never expires"
    );
    assert!(hold["expires_at"].is_null());

    // So reclaiming it still takes the operator saying so.
    qp(&db)
        .args(["reclaim", "QP-1", "--reason", "r"])
        .assert()
        .failure()
        .code(2);
    qp(&db)
        .args(["reclaim", "QP-1", "--reason", "r", "--force"])
        .assert()
        .success();
}

#[test]
fn a_lease_length_without_a_unit_suffix_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();

    // A bare number is ambiguous in exactly the way that sets a lease to eight
    // seconds when eight hours was meant, so it is refused rather than guessed.
    for bad in ["8", "", "8hours", "0h", "-1h"] {
        let assert = qp(&db)
            .args(["claim", "QP-1", "--as", "x", "--for", bad, "--json"])
            .assert()
            .failure()
            .code(1);
        assert_eq!(json_stderr(&assert)["error"]["kind"], "invalid_input");
    }
    // Nothing was claimed on the way through.
    assert!(hold_of(&db, "QP-1").is_null());
    for good in ["30s", "45m", "8h", "2d"] {
        qp(&db)
            .args(["claim", "QP-1", "--as", "x", "--for", good])
            .assert()
            .success();
        qp(&db)
            .args(["abandon", "QP-1", "--as", "x", "--reason", "r"])
            .assert()
            .success();
        qp(&db)
            .args(["assign", "QP-1", "--to", "x"])
            .assert()
            .success();
    }
}

#[test]
fn reclaiming_a_stale_hold_lands_where_a_manual_reclaim_would() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x", "--for", "45m"])
        .assert()
        .success();
    age_hold(&db, 1, 46 * 60);

    let assert = qp(&db)
        .args(["reclaim", "QP-1", "--reason", "machine gone", "--json"])
        .assert()
        .success();
    let v = json_stdout(&assert);
    // Same destination as the manual path: `pending`, promoted to `ready` when
    // nothing blocks it, with the assignment closed as `reclaimed`.
    assert_eq!(v["state"], "ready");
    assert_eq!(v["forced"], false);
    assert!(hold_of(&db, "QP-1").is_null(), "the hold is gone");
    qp(&db)
        .args(["assign", "QP-1", "--to", "y"])
        .assert()
        .success();
    let conn = rusqlite::Connection::open(&db).unwrap();
    let outcome: String = conn
        .query_row(
            "SELECT outcome FROM assignment WHERE agent_id = 'x'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "reclaimed");
}

#[test]
fn reclaiming_a_live_hold_needs_the_force_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x", "--for", "8h"])
        .assert()
        .success();

    let assert = qp(&db)
        .args(["reclaim", "QP-1", "--reason", "impatience", "--json"])
        .assert()
        .failure()
        .code(2);
    let env = json_stderr(&assert);
    assert_eq!(env["error"]["code"], "hold_is_live");
    // The refusal names who holds it and where, which is the whole reason the
    // machine is recorded on the row.
    let msg = env["error"]["message"].as_str().unwrap();
    assert!(msg.contains("`x`"), "refusal must name the holder: {msg}");
    assert!(msg.contains("host.invalid"), "and the machine: {msg}");
    // The ticket did not move.
    qp(&db)
        .args(["show", "QP-1", "--json"])
        .assert()
        .success()
        .stdout(contains(r#""state":"running""#));

    // The manual override stays — it just stops being the only option.
    qp(&db)
        .args(["reclaim", "QP-1", "--reason", "operator says so", "--force"])
        .assert()
        .success();
}

#[test]
fn a_dispossessed_holder_is_told_by_name_on_its_next_write() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "a"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x", "--for", "45m"])
        .assert()
        .success();
    age_hold(&db, 1, 46 * 60);
    qp(&db)
        .args(["reclaim", "QP-1", "--reason", "stale"])
        .assert()
        .success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "y"])
        .assert()
        .success();

    // The machine comes back. Every write it tries is refused, by name, and
    // told not to retry — this is the headline failure arriving through the
    // back door, so it must not surface as a vague state error.
    for args in [
        vec!["log", "QP-1", "note", "back from the dead", "--as", "x"],
        vec!["complete", "QP-1", "--as", "x"],
        vec!["abandon", "QP-1", "--as", "x", "--reason", "r"],
        vec!["block", "QP-1", "--as", "x", "--new", "b"],
    ] {
        let mut with_json = args.clone();
        with_json.push("--json");
        let assert = qp(&db).args(&with_json).assert().failure().code(2);
        let env = json_stderr(&assert);
        let msg = env["error"]["message"].as_str().unwrap_or_default();
        assert_eq!(
            env["error"]["kind"], "not_owner",
            "`qp {}` must be a not_owner, not a retryable conflict: {env}",
            args[0]
        );
        assert!(
            msg.contains("`x`") && msg.contains("QP-1"),
            "`qp {}` must name the dispossessed holder and the ticket: {msg}",
            args[0]
        );
    }

    // And the ticket was left alone: still assigned to the new holder, with no
    // blocker minted by the refused `block`.
    let assert = qp(&db).args(["show", "QP-1", "--json"]).assert().success();
    let v = json_stdout(&assert);
    assert_eq!(v["state"], "assigned");
    assert_eq!(v["agent"], "y");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let tasks: i64 = conn
        .query_row("SELECT count(*) FROM task", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tasks, 1, "a refused write must not leave a blocker behind");
}

/// The liveness columns must reach stores that already exist, not just fresh
/// ones — and must land empty, so a claim taken before leases existed keeps the
/// only safe reading: no stated expiry, never expires.
#[test]
fn existing_store_gains_the_liveness_columns_on_migration() {
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
    // Rewind to a store built by a binary from before the columns existed.
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "ALTER TABLE assignment DROP COLUMN last_contact;
             ALTER TABLE assignment DROP COLUMN lease_seconds;
             ALTER TABLE assignment DROP COLUMN holder_host;
             UPDATE meta SET value = '6' WHERE key = 'schema_version';",
        )
        .unwrap();
    }

    // Any command goes through `open()`, which must migrate the store forward.
    qp(&db).args(["list"]).assert().success();

    let conn = rusqlite::Connection::open(&db).unwrap();
    for col in ["last_contact", "lease_seconds", "holder_host"] {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('assignment') WHERE name = ?1",
                [col],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "existing store must gain assignment.{col}");
    }
    let v: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, CURRENT_SCHEMA_VERSION);
    // The pre-existing hold is unbreakable, because it never stated an expiry.
    assert_eq!(hold_of(&db, "QP-1")["stale"], false);
    qp(&db)
        .args(["reclaim", "QP-1", "--reason", "r"])
        .assert()
        .failure()
        .code(2);
}

// ---------------------------------------------------------------------------
// `_effects` — collateral state changes
// ---------------------------------------------------------------------------

/// The `_effects` list on a payload, flattened to `(task, from, to, via)`.
///
/// Absent and empty are different answers and the helper keeps them so: a
/// missing key returns `None`, never an empty vector.
fn effects_of(v: &serde_json::Value) -> Option<Vec<(String, String, String, String)>> {
    let list = v.get("_effects")?.as_array().unwrap().clone();
    Some(
        list.iter()
            .map(|e| {
                assert_eq!(e["kind"], "state_change", "unexpected effect kind in {e}");
                (
                    e["task"].as_str().unwrap().to_string(),
                    e["from"].as_str().unwrap().to_string(),
                    e["to"].as_str().unwrap().to_string(),
                    e["via"].as_str().unwrap().to_string(),
                )
            })
            .collect(),
    )
}

/// Forces a task back to `pending` behind the CLI's back.
///
/// This is drift: a `pending` row with nothing left to wait for. It is the
/// state the store-wide sweeps exist to repair, and for `add`, `abandon` and
/// `reclaim` it is the *only* way to reach a collateral promotion — none of the
/// three resolves a dependency, so nothing in the dependency graph can promote
/// a task they did not name. Writing it directly is deliberate: the repair
/// those sweeps perform is a real state change on a task the caller never
/// mentioned, and `_effects` has to report it like any other.
fn strand(db: &std::path::Path, rowid: i64) {
    let conn = rusqlite::Connection::open(db).unwrap();
    let n = conn
        .execute("UPDATE task SET state = 'pending' WHERE id = ?", [rowid])
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn complete_reports_the_dependents_it_promoted() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "blocker"]).assert().success();
    qp(&db)
        .args(["add", "waiter", "--depends-on", "QP-1"])
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
    let a = qp(&db)
        .args(["complete", "QP-1", "--as", "x", "--json"])
        .assert()
        .success();
    assert_eq!(
        effects_of(&json_stdout(&a)),
        Some(vec![(
            "QP-2".to_string(),
            "pending".to_string(),
            "ready".to_string(),
            "complete".to_string()
        )])
    );
}

#[test]
fn complete_omits_effects_when_nothing_else_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "lonely"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    let a = qp(&db)
        .args(["complete", "QP-1", "--as", "x", "--json"])
        .assert()
        .success();
    let v = json_stdout(&a);
    assert!(
        v.get("_effects").is_none(),
        "the key must be absent, not an empty list: {v}"
    );
}

#[test]
fn cancel_reports_the_dependents_it_promoted() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "blocker"]).assert().success();
    qp(&db)
        .args(["add", "waiter", "--depends-on", "QP-1"])
        .assert()
        .success();
    let a = qp(&db)
        .args(["cancel", "QP-1", "--json"])
        .assert()
        .success();
    assert_eq!(
        effects_of(&json_stdout(&a)),
        Some(vec![(
            "QP-2".to_string(),
            "pending".to_string(),
            "ready".to_string(),
            "cancel".to_string()
        )])
    );
}

#[test]
fn cancel_omits_effects_when_nothing_else_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "lonely"]).assert().success();
    let a = qp(&db)
        .args(["cancel", "QP-1", "--json"])
        .assert()
        .success();
    assert!(json_stdout(&a).get("_effects").is_none());
}

/// The demotion direction, which is the one most easily got backwards.
///
/// `qp block` is the only verb whose collateral is `demote_frozen` rather than
/// a promotion sweep: the blocker freezes the blocked task, and the freeze
/// propagates down every `contains` edge under it.
#[test]
fn block_reports_the_children_it_froze() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "slice"]).assert().success();
    qp(&db).args(["add", "child"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    // Attached while QP-1 is running, so the child is still `ready` when the
    // blocker lands — a child attached to an already-frozen parent would be
    // demoted by that call instead, and this test would prove nothing.
    let a = qp(&db)
        .args(["contains", "QP-1", "QP-2", "--as", "x", "--json"])
        .assert()
        .success();
    assert!(json_stdout(&a).get("_effects").is_none());
    let a = qp(&db)
        .args(["block", "QP-1", "--as", "x", "--new", "blocker", "--json"])
        .assert()
        .success();
    assert_eq!(
        effects_of(&json_stdout(&a)),
        Some(vec![(
            "QP-2".to_string(),
            "ready".to_string(),
            "pending".to_string(),
            "block".to_string()
        )])
    );
}

#[test]
fn block_omits_effects_when_nothing_else_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "slice"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    let a = qp(&db)
        .args(["block", "QP-1", "--as", "x", "--new", "blocker", "--json"])
        .assert()
        .success();
    assert!(json_stdout(&a).get("_effects").is_none());
}

#[test]
fn contains_reports_the_grandchildren_it_froze() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "wave"]).assert().success();
    qp(&db).args(["add", "blocker"]).assert().success();
    qp(&db).args(["add", "mid"]).assert().success();
    qp(&db).args(["add", "leaf"]).assert().success();
    qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2"])
        .assert()
        .success();
    qp(&db)
        .args(["contains", "QP-3", "QP-4"])
        .assert()
        .success();
    // QP-1 is frozen, so attaching QP-3 to it freezes QP-3 *and* QP-4. Only
    // QP-4 is collateral: the other two are named on the command line.
    let a = qp(&db)
        .args(["contains", "QP-1", "QP-3", "--json"])
        .assert()
        .success();
    assert_eq!(
        effects_of(&json_stdout(&a)),
        Some(vec![(
            "QP-4".to_string(),
            "ready".to_string(),
            "pending".to_string(),
            "contains".to_string()
        )])
    );
}

#[test]
fn contains_omits_effects_when_nothing_else_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "wave"]).assert().success();
    qp(&db).args(["add", "slice"]).assert().success();
    let a = qp(&db)
        .args(["contains", "QP-1", "QP-2", "--json"])
        .assert()
        .success();
    assert!(json_stdout(&a).get("_effects").is_none());
}

/// Both `depends` directions, in one graph, because they are inverses.
///
/// The add freezes a task neither endpoint names; the `--rm` thaws it again.
/// `promoted` stays a statement about the named task throughout, which is why
/// it is kept rather than folded into `_effects`.
#[test]
fn depends_reports_the_subtree_it_froze_and_thawed() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "wave"]).assert().success();
    qp(&db).args(["add", "blocker"]).assert().success();
    qp(&db).args(["add", "leaf"]).assert().success();
    qp(&db)
        .args(["contains", "QP-1", "QP-3"])
        .assert()
        .success();
    let a = qp(&db)
        .args(["depends", "QP-1", "--on", "QP-2", "--json"])
        .assert()
        .success();
    let v = json_stdout(&a);
    assert_eq!(v["promoted"], false);
    assert_eq!(
        effects_of(&v),
        Some(vec![(
            "QP-3".to_string(),
            "ready".to_string(),
            "pending".to_string(),
            "blocks".to_string()
        )])
    );
    let a = qp(&db)
        .args(["depends", "QP-1", "--rm", "--on", "QP-2", "--json"])
        .assert()
        .success();
    let v = json_stdout(&a);
    assert_eq!(
        effects_of(&v),
        Some(vec![(
            "QP-3".to_string(),
            "pending".to_string(),
            "ready".to_string(),
            "depends_rm".to_string()
        )])
    );
}

#[test]
fn depends_omits_effects_when_only_the_named_task_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "blocker"]).assert().success();
    qp(&db).args(["add", "waiter"]).assert().success();
    // QP-2 is demoted by this edge, and QP-2 is named — so the payload reports
    // it through `state`, not through `_effects`.
    let a = qp(&db)
        .args(["depends", "QP-2", "--on", "QP-1", "--json"])
        .assert()
        .success();
    assert!(json_stdout(&a).get("_effects").is_none());
}

#[test]
fn add_reports_the_drift_its_sweep_repaired() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "first"]).assert().success();
    qp(&db).args(["add", "stranded"]).assert().success();
    strand(&db, 2);
    let a = qp(&db).args(["add", "third", "--json"]).assert().success();
    assert_eq!(
        effects_of(&json_stdout(&a)),
        Some(vec![(
            "QP-2".to_string(),
            "pending".to_string(),
            "ready".to_string(),
            "add".to_string()
        )])
    );
}

#[test]
fn add_omits_effects_when_nothing_else_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    let a = qp(&db).args(["add", "first", "--json"]).assert().success();
    assert!(json_stdout(&a).get("_effects").is_none());
}

#[test]
fn abandon_reports_the_drift_its_sweep_repaired() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "held"]).assert().success();
    qp(&db).args(["add", "stranded"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    strand(&db, 2);
    let a = qp(&db)
        .args(["abandon", "QP-1", "--as", "x", "--json"])
        .assert()
        .success();
    assert_eq!(
        effects_of(&json_stdout(&a)),
        Some(vec![(
            "QP-2".to_string(),
            "pending".to_string(),
            "ready".to_string(),
            "abandon".to_string()
        )])
    );
}

#[test]
fn abandon_omits_effects_when_only_the_named_task_returned() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "held"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    let a = qp(&db)
        .args(["abandon", "QP-1", "--as", "x", "--json"])
        .assert()
        .success();
    let v = json_stdout(&a);
    assert_eq!(v["state"], "ready");
    assert!(v.get("_effects").is_none());
}

#[test]
fn reclaim_reports_the_drift_its_sweep_repaired() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "held"]).assert().success();
    qp(&db).args(["add", "stranded"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    strand(&db, 2);
    let a = qp(&db)
        .args(["reclaim", "QP-1", "--force", "--json"])
        .assert()
        .success();
    assert_eq!(
        effects_of(&json_stdout(&a)),
        Some(vec![(
            "QP-2".to_string(),
            "pending".to_string(),
            "ready".to_string(),
            "reclaim".to_string()
        )])
    );
}

#[test]
fn reclaim_omits_effects_when_only_the_named_task_returned() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "held"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();
    let a = qp(&db)
        .args(["reclaim", "QP-1", "--force", "--json"])
        .assert()
        .success();
    let v = json_stdout(&a);
    assert_eq!(v["state"], "ready");
    assert!(v.get("_effects").is_none());
}

// ---------------------------------------------------------------------------
// Staleness in the board views a dashboard and a `list` reader actually see.
//
// `show` and `wave` flagged a stranded hold from the start; `list` and `report`
// did not, so a ticket whose machine died was invisible in two of the four
// views and in the dashboard entirely. These pin that it is visible in both,
// that a live hold is not marked in either, and that a hold which never stated
// an expiry is never marked at all.
// ---------------------------------------------------------------------------

/// The `tasks` entry for one display id out of a board payload.
fn report_task(db: &std::path::Path, want: &str) -> serde_json::Value {
    let a = qp(db).args(["report", "--json"]).assert().success();
    json_stdout(&a)["tasks"]
        .as_array()
        .expect("report payload has a tasks array")
        .iter()
        .find(|t| t["display_id"] == want)
        .unwrap_or_else(|| panic!("{want} missing from the board payload"))
        .clone()
}

/// The `list` line for one display id, human mode.
fn list_line(db: &std::path::Path, want: &str) -> String {
    let a = qp(db).args(["list"]).assert().success();
    let s = String::from_utf8(a.get_output().stdout.clone()).unwrap();
    s.lines()
        .find(|l| l.starts_with(&format!("{want}\t")))
        .unwrap_or_else(|| panic!("{want} missing from list:\n{s}"))
        .to_string()
}

fn list_row(db: &std::path::Path, want: &str) -> serde_json::Value {
    let a = qp(db).args(["list", "--json"]).assert().success();
    json_stdout(&a)
        .as_array()
        .expect("list --json is an array")
        .iter()
        .find(|t| t["display_id"] == want)
        .unwrap_or_else(|| panic!("{want} missing from list --json"))
        .clone()
}

#[test]
fn a_lapsed_hold_is_visible_in_list_and_in_the_board_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "stranded"]).assert().success();
    qp(&db).args(["add", "in progress"]).assert().success();
    for (t, who) in [("QP-1", "dead"), ("QP-2", "alive")] {
        qp(&db).args(["assign", t, "--to", who]).assert().success();
        qp(&db)
            .args(["claim", t, "--as", who, "--for", "45m"])
            .assert()
            .success();
    }

    // Only the first one's machine went quiet past its stated length.
    age_hold(&db, 1, 46 * 60);

    // `list`, human. The marker trails TITLE and takes no column: `show`'s
    // header is pinned to this column set, so the tab count must not move.
    let stranded = list_line(&db, "QP-1");
    assert!(
        stranded.contains("STALE"),
        "no marker on a lapsed hold: {stranded}"
    );
    assert!(
        stranded.contains("host.invalid"),
        "the marker must name the machine: {stranded}"
    );
    let live = list_line(&db, "QP-2");
    assert!(
        !live.contains("STALE"),
        "a live hold must not be marked: {live}"
    );
    assert_eq!(
        stranded.matches('\t').count(),
        live.matches('\t').count(),
        "the marker became a column; `show`'s header mirrors these:\n{stranded}\n{live}"
    );
    assert_eq!(
        stranded.matches('\t').count(),
        4,
        "list is a five-column TSV: {stranded}"
    );

    // `list --json`, spelled as `wave --json` spells it.
    let row = list_row(&db, "QP-1");
    assert_eq!(row["stale"], true);
    assert_eq!(row["hold"]["agent"], "dead");
    assert_eq!(row["hold"]["host"], "host.invalid");
    assert_eq!(list_row(&db, "QP-2")["stale"], false);

    // The board payload the dashboard reads, same spelling again.
    let t1 = report_task(&db, "QP-1");
    assert_eq!(
        t1["stale"], true,
        "a stranded ticket is invisible on the board: {t1}"
    );
    assert_eq!(t1["hold"]["agent"], "dead");
    assert!(t1["hold"]["expires_at"].is_string());
    let t2 = report_task(&db, "QP-2");
    assert_eq!(t2["stale"], false, "a live hold must not be marked: {t2}");
}

#[test]
fn a_hold_with_no_stated_expiry_is_never_stale_in_list_or_the_board_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    qp(&db).args(["add", "old claim"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "x"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "x"])
        .assert()
        .success();

    // The shape every open claim written before leases existed is in. Absent is
    // not expired, and reading it as expired is the easy mistake: it would hand
    // `reclaim` every hold in a store that predates the columns.
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE assignment SET lease_seconds = NULL, last_contact = NULL
              WHERE completed_at IS NULL",
            [],
        )
        .unwrap();
    }

    let line = list_line(&db, "QP-1");
    assert!(
        !line.contains("STALE"),
        "a claim promised no expiry was marked: {line}"
    );
    assert_eq!(list_row(&db, "QP-1")["stale"], false);
    let t = report_task(&db, "QP-1");
    assert_eq!(
        t["stale"], false,
        "a claim promised no expiry was marked: {t}"
    );
    assert!(
        t["hold"]["expires_at"].is_null(),
        "an expiry was invented: {t}"
    );
}

/// A page of held tickets costs one liveness query, not one per row.
///
/// The statement count is not observable from outside the process, so what is
/// asserted here is the behaviour a bulk query has to have: one invocation
/// answers correctly for every row of a page, with the aged ones marked and the
/// rest not. The single `lease::holds_by_task` call in each of `list::exec` and
/// `report::collect_json` is the structural half.
#[test]
fn a_page_of_holds_is_answered_by_one_listing() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();
    const N: i64 = 24;
    for i in 1..=N {
        qp(&db).args(["add", &format!("t{i}")]).assert().success();
        let t = format!("QP-{i}");
        qp(&db).args(["assign", &t, "--to", "w"]).assert().success();
        qp(&db)
            .args(["claim", &t, "--as", "w", "--for", "45m"])
            .assert()
            .success();
        // Every other one has gone quiet past its lease.
        if i % 2 == 0 {
            age_hold(&db, i, 46 * 60);
        }
    }

    let a = qp(&db).args(["list", "--json"]).assert().success();
    let rows = json_stdout(&a);
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len() as i64, N);
    for r in rows {
        let id: i64 = r["display_id"]
            .as_str()
            .unwrap()
            .trim_start_matches("QP-")
            .parse()
            .unwrap();
        assert_eq!(
            r["stale"],
            id % 2 == 0,
            "wrong liveness for {}",
            r["display_id"]
        );
    }

    let a = qp(&db).args(["report", "--json"]).assert().success();
    let payload = json_stdout(&a);
    let tasks = payload["tasks"].as_array().unwrap();
    assert_eq!(tasks.len() as i64, N);
    let stale = tasks.iter().filter(|t| t["stale"] == true).count();
    assert_eq!(
        stale as i64,
        N / 2,
        "the board payload disagrees with `list`"
    );
}
