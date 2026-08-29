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
//!
//! `claim` and `assign` guard on a state literal. `cancel`, `abandon`, `reclaim`
//! and `block` cannot: their guards name a *set* of legal origins, so they read
//! the row before the guarded `UPDATE` to put the origin in the event payload.
//! That read is safe only because the transaction is `IMMEDIATE` — the write
//! lock is held before the `SELECT`, so nothing can move the row between the two
//! statements. `concurrent_cancels_produce_exactly_one_winner` races one of
//! those commands, and `with_tx_uses_immediate_transactions` pins the property
//! the whole arrangement rests on.

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
    race_n(db, args, RACERS)
}

/// [`race`] with the racer count named, for a case that wants a smaller field.
fn race_n(db: &Path, args: &[&str], racers: usize) -> Vec<RaceResult> {
    let children: Vec<Child> = (0..racers)
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

/// A command that reads the row before mutating still produces one winner.
///
/// `cancel` guards on `state NOT IN ('done','cancelled')`, a set rather than a
/// literal, so it reads the current state first to name the transition's origin
/// in the event. The read is payload-only — nothing branches on it — and this
/// asserts the guard behind it is still the only thing deciding who wins.
#[test]
fn concurrent_cancels_produce_exactly_one_winner() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();

    for round in 1..=ROUNDS {
        let task = format!("QP-{round}");
        qp(&db).args(["add", "race"]).assert().success();

        let results = race(&db, &["cancel", &task, "--reason", "r"]);
        tally(&results, "already_terminal");

        assert_eq!(
            state_change_count(&db, &task, "cancelled"),
            1,
            "exactly one cancellation may land on {task}"
        );
        // And exactly one of them recorded an origin — if two racers had both
        // read `ready` and both written, this would be 2 with the guard intact
        // only by luck.
        let out = qp(&db)
            .args(["timeline", &task, "--json", "--kind", "state_change"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let events: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        let origins: Vec<&str> = events
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["payload"]["to"] == "cancelled")
            .filter_map(|e| e["payload"]["from"].as_str())
            .collect();
        assert_eq!(
            origins,
            vec!["ready"],
            "the surviving cancellation names the state the winner actually found"
        );
    }
}

/// `with_tx` opens `IMMEDIATE` transactions, and this is what says so.
///
/// Asserted against source because `qp` is a bin-only crate with nothing for an
/// integration test to import, the same reason `tests/docs.rs` reads source.
///
/// It is worth a test of its own because nothing else fails without it. Flip
/// `with_tx` to the `DEFERRED` default and every test in this file still passes
/// — the races resolve by `SQLITE_BUSY` and retry rather than by the write lock
/// — while the payload reads in `cancel`, `abandon`, `reclaim` and `block`
/// quietly stop being safe: with a deferred transaction the `SELECT` runs
/// outside the write lock, so another writer can move the row between it and the
/// guarded `UPDATE`. The transition would still be correct, because the guard is
/// still the only authority on it, but the event could name an origin that was
/// already stale when it was written, and a log that misreports history is the
/// exact defect this file's neighbours exist to prevent.
#[test]
fn with_tx_uses_immediate_transactions() {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("db.rs"),
    )
    .expect("read src/db.rs");
    let body = src
        .split_once("pub fn with_tx<T>")
        .expect("with_tx still exists in db.rs")
        .1;
    let body = &body[..body.find("\n}\n").expect("with_tx has a body")];
    assert!(
        body.contains("TransactionBehavior::Immediate"),
        "with_tx must open IMMEDIATE transactions — the payload reads in cancel, \
         abandon, reclaim and block are only safe while the write lock is held \
         before the SELECT. Body was:\n{body}"
    );
}

/// Machines that all decide one dead hold is theirs to break produce one winner.
///
/// This is the property the whole liveness mechanism rests on, and it is the
/// one an expiry check makes easy to lose: read "is it stale", then release, and
/// two machines that both read `stale` both release — the second one breaking a
/// hold the first had already handed to somebody else. So staleness lives
/// *inside* the guarded `UPDATE` rather than in a check before it, and the same
/// `changes() == 1` that decides every other transition decides this one.
///
/// Four processes, not the module's usual six: the point is overlap, and this
/// test builds its store first, so a wider field buys nothing.
#[test]
fn concurrent_reclaims_of_one_stale_hold_produce_exactly_one_winner() {
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
        qp(&db)
            .args(["claim", &task, "--as", "agent", "--for", "45m"])
            .assert()
            .success();
        // Quiet for longer than the lease it took. No sleeping: the stored
        // length is what lets a test construct a past expiry directly.
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE assignment
                SET last_contact = strftime('%Y-%m-%dT%H:%M:%fZ','now','-46 minutes')
              WHERE completed_at IS NULL",
            [],
        )
        .unwrap();
        drop(conn);

        let results = race_n(&db, &["reclaim", &task, "--reason", "dead"], 4);
        let winners = results.iter().filter(|r| r.code == 0).count();
        assert_eq!(winners, 1, "exactly one machine may break one dead hold");
        for l in results.iter().filter(|r| r.code != 0) {
            assert_eq!(l.code, 2);
            // The losers arrive after the task has left `running`; a loser that
            // reported `hold_is_live` would mean it had raced the guard and lost
            // on staleness instead, which is a different bug.
            assert_eq!(l.error_code.as_deref(), Some("not_assigned_or_running"));
        }
        assert_eq!(
            state_change_count(&db, &task, "pending"),
            1,
            "exactly one release may land on {task}"
        );
    }
}
