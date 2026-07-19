//! Concurrency tests for the guarded state transitions.
//!
//! `tests/cli.rs` exercises the conflict paths sequentially — claim, then claim
//! again — which only proves the guard rejects a *late* writer. These tests
//! start N `qp` processes against one task *before* waiting on any of them, so
//! the writers genuinely overlap, and assert the invariant the whole design
//! rests on: exactly one winner, N-1 conflicts, one state transition recorded.
//!
//! The assertions are interleaving-independent — "exactly one winner" holds
//! whether the children overlap or accidentally serialise — so the test cannot
//! be flaky. Overlap is maximised (not relied upon) by spawning every child up
//! front and by using several rounds with N > 2.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};

/// Number of racers per round. Process spawn is ~1 ms while a `qp` run is
/// ~10-30 ms, so with 6 children the later spawns land while the earlier ones
/// are still inside `BEGIN IMMEDIATE`.
const RACERS: usize = 6;
const ROUNDS: usize = 3;

fn qp(db: &Path) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env("QP_DB", db);
    c
}

fn qp_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("qp")
}

/// One `qp` invocation's result: exit code plus the stable error code, if any.
struct RaceResult {
    code: i32,
    error_code: Option<String>,
}

/// Start every child before waiting on any of them, then collect.
fn race(db: &Path, args: &[&str]) -> Vec<RaceResult> {
    let children: Vec<Child> = (0..RACERS)
        .map(|_| {
            std::process::Command::new(qp_bin())
                .args(args)
                .arg("--json")
                .env("QP_DB", db)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();

    children
        .into_iter()
        .map(|ch| {
            let out = ch.wait_with_output().unwrap();
            RaceResult {
                code: out.status.code().unwrap(),
                error_code: error_code(&out.stderr),
            }
        })
        .collect()
}

/// Under `--json`, stderr is JSON Lines: zero or more `{"warning":...}` then at
/// most one `{"error":...}`. Parse line-wise, never whole-buffer.
fn error_code(stderr: &[u8]) -> Option<String> {
    std::str::from_utf8(stderr)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| v["error"]["code"].as_str().map(str::to_string))
}

/// Count `state_change` events on a task whose payload says `to == state`.
fn state_change_count(db: &Path, task: &str, state: &str) -> usize {
    let out = qp(db)
        .args(["timeline", task, "--json", "--kind", "state_change"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let events: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["payload"]["to"] == state)
        .count()
}

fn tally(results: &[RaceResult], loser_code: &str) {
    let winners = results.iter().filter(|r| r.code == 0).count();
    let losers: Vec<&RaceResult> = results.iter().filter(|r| r.code != 0).collect();
    assert_eq!(winners, 1, "expected exactly one winner, got {winners}");
    assert_eq!(losers.len(), RACERS - 1);
    for l in losers {
        assert_eq!(l.code, 2, "loser must exit 2 (constraint violation)");
        assert_eq!(
            l.error_code.as_deref(),
            Some(loser_code),
            "loser must report the stable conflict code"
        );
    }
    for w in results.iter().filter(|r| r.code == 0) {
        assert_eq!(w.error_code, None, "winner must not emit an error line");
    }
}

#[test]
fn concurrent_claims_produce_exactly_one_winner() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();

    for round in 1..=ROUNDS {
        let task = format!("QP-{round}");
        qp(&db).args(["add", "race"]).assert().success();
        qp(&db)
            .args(["assign", &task, "--to", "agent"])
            .assert()
            .success();

        let results = race(&db, &["claim", &task, "--as", "agent"]);
        tally(&results, "already_claimed");

        // The claim's `UPDATE assignment SET claimed_at` and this event are
        // written in the same guarded transaction, so exactly one
        // `to: running` event <=> exactly one non-null `claimed_at`.
        assert_eq!(
            state_change_count(&db, &task, "running"),
            1,
            "exactly one claim may land on {task}"
        );
        // And the task is running, owned by the single winner.
        qp(&db)
            .args(["show", &task, "--json"])
            .assert()
            .success()
            .stdout(predicates::str::contains(r#""state":"running""#));
    }
}

#[test]
fn concurrent_assigns_produce_exactly_one_winner() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();

    for round in 1..=ROUNDS {
        let task = format!("QP-{round}");
        qp(&db).args(["add", "race"]).assert().success();

        // All racers target the same agent so the only difference between them
        // is who gets there first.
        let results = race(&db, &["assign", &task, "--to", "agent"]);
        // The loser trips the `WHERE state = 'ready'` guard on `task`.
        tally(&results, "not_ready");

        assert_eq!(
            state_change_count(&db, &task, "assigned"),
            1,
            "exactly one assignment may land on {task}"
        );
    }
}
