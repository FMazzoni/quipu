//! `qp remote` — the config surface, and the local-only state it exists to leave and return to.
//!
//! The ledger is an object-store url, and most of these are config-only tests:
//! nothing here contacts it. The two that probe point at a loopback port
//! nothing is listening on. Every name is RFC 2606 or `127.0.0.1`, no
//! credential is real, and no host or account belonging to anyone appears in
//! this file, which is deliberate — the repo is public.
//!
//! The load-bearing tests here are the ones about *absence*. Local-only is the
//! default and the sanctioned escape hatch from an unreachable ledger, so it
//! has to stay a first-class state rather than a path that rots from disuse.

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::Value;

// ---------------------------------------------------------------------------
// round-trip
// ---------------------------------------------------------------------------

#[test]
fn set_show_round_trips_the_url() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let ledger = ledger_url();
    init(&db);

    set_ledger(&db, &ledger);

    let v = show_json(&db, &[]);
    assert_eq!(v["ledger"], Value::from(ledger));
    assert_eq!(v["local_only"], Value::from(false));
}

#[test]
fn a_later_set_replaces_rather_than_appends() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);

    set_ledger(&db, "s3://minio.host.example/first");
    set_ledger(&db, "s3://minio.host.example/second");

    assert_eq!(
        show_json(&db, &[])["ledger"],
        Value::from("s3://minio.host.example/second")
    );
}

#[test]
fn a_ledger_url_is_a_complete_configuration() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let ledger = ledger_url();
    init(&db);

    set_ledger(&db, &ledger);

    let v = show_json(&db, &[]);
    assert_eq!(v["ledger"], Value::from(ledger));
    assert_eq!(v["local_only"], Value::from(false));
}

#[test]
fn set_with_no_flag_is_invalid_input() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);
    qp(&db)
        .args(["remote", "set"])
        .assert()
        .code(1)
        .stderr(contains("--ledger"));
}

// ---------------------------------------------------------------------------
// absent config behaves exactly as today
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_store_is_local_only() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);

    let v = show_json(&db, &[]);
    assert_eq!(v["local_only"], Value::from(true));
    assert_eq!(v["ledger"], Value::Null);

    qp(&db)
        .args(["remote", "show"])
        .assert()
        .success()
        .stdout(contains("local-only"));
}

/// The whole lifecycle, on a store that has never heard of a remote.
///
/// This is the test that says "absent config, behaviour is exactly as today":
/// every state transition succeeds, and nothing outside the store file is
/// created, because nothing spawned a child or opened a socket at all.
#[test]
fn with_no_remote_the_lifecycle_is_untouched_and_nothing_is_spawned() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);

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
    qp(&db).args(["status"]).assert().success();

    assert!(
        siblings_of(&db).iter().all(|n| n.starts_with("db.sqlite")),
        "a store with no remote created something beside the db: {:?}",
        siblings_of(&db)
    );
}

/// A configured ledger still leaves every other command local.
///
/// The url here names a loopback port nothing is listening on, so anything that
/// tried to reach it would fail. Nothing does: only `sync` and an explicit
/// `--check` ever leave the machine.
#[test]
fn a_configured_remote_does_not_make_ordinary_commands_reach_out() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);
    set_ledger(&db, &dead_url());

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
    qp(&db).args(["list"]).assert().success();
    qp(&db).args(["remote", "show"]).assert().success();
}

// ---------------------------------------------------------------------------
// clear — the escape hatch
// ---------------------------------------------------------------------------

#[test]
fn bare_clear_returns_the_store_to_local_only() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);
    set_ledger(&db, &ledger_url());

    qp(&db)
        .args(["remote", "clear"])
        .assert()
        .success()
        .stdout(contains("local-only"));

    let v = show_json(&db, &[]);
    assert_eq!(v["local_only"], Value::from(true));
    assert_eq!(v["ledger"], Value::Null);
}

/// Every spelling of `clear` clears what is there.
///
/// Bare, `--all` and naming the url are three ways to say the same thing while
/// a store holds one url. They are asserted together because the whole point of
/// `clear` is that someone reaching for it is already blocked: whichever form
/// they reach for has to work.
#[test]
fn all_and_naming_the_url_are_the_same_as_bare_clear() {
    for flags in [
        vec!["remote", "clear"],
        vec!["remote", "clear", "--all"],
        vec!["remote", "clear", "--ledger"],
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        init(&db);
        set_ledger(&db, &ledger_url());

        qp(&db).args(&flags).assert().success();
        assert_eq!(
            show_json(&db, &[])["local_only"],
            Value::from(true),
            "`qp {}` left the store attached",
            flags.join(" ")
        );
    }
}

/// Clearing what is not set is a no-op, not an error.
///
/// Someone reaching for `clear` is already blocked; making them find out
/// whether anything was configured would be the wrong moment to be strict.
#[test]
fn clearing_an_unset_store_succeeds_and_reports_nothing_cleared() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);

    let out = qp(&db)
        .args(["remote", "clear", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["cleared"], Value::Array(vec![]));
    assert_eq!(v["local_only"], Value::from(true));
}

/// The full round trip the design turns on: attach, detach, attach again.
#[test]
fn local_only_survives_a_full_attach_detach_reattach_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let ledger = ledger_url();
    init(&db);

    assert_eq!(show_json(&db, &[])["local_only"], Value::from(true));
    set_ledger(&db, &ledger);
    assert_eq!(show_json(&db, &[])["local_only"], Value::from(false));
    qp(&db).args(["remote", "clear"]).assert().success();
    assert_eq!(show_json(&db, &[])["local_only"], Value::from(true));
    set_ledger(&db, &ledger);
    assert_eq!(show_json(&db, &[])["ledger"], Value::from(ledger));
}

// ---------------------------------------------------------------------------
// the opt-in reachability check
// ---------------------------------------------------------------------------

/// Setting a remote that is down must still write the config.
///
/// This is the property that keeps `set` honest: configuration is a statement
/// of intent, and the far end being unavailable right now says nothing about
/// whether the intent is correct.
#[test]
fn an_unreachable_remote_is_reported_but_still_configured() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);
    let dead = dead_url();

    let out = reachable_qp(&db)
        .args(["remote", "set", "--ledger", &dead, "--check", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a failed probe must not fail the write"
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let why = v["ledger_reachable"]["fault"]
        .as_str()
        .unwrap_or_else(|| panic!("no fault reported: {v}"));
    assert!(
        why.contains("unreachable"),
        "a dead endpoint was not reported as unreachable: {why}"
    );
    assert_eq!(v["ledger"], Value::from(dead.clone()));

    // And it is genuinely persisted, not merely echoed back.
    assert_eq!(show_json(&db, &[])["ledger"], Value::from(dead));
}

#[test]
fn without_check_no_reachability_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);
    set_ledger(&db, &dead_url());

    let v = show_json(&db, &[]);
    assert!(v.get("ledger_reachable").is_none());
}

/// `--check` probes what is configured and nothing else.
///
/// An unconfigured store must spawn nothing even when asked to check, which is
/// what keeps `--check` safe to put in a script that runs everywhere.
#[test]
fn check_only_probes_what_is_configured() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);

    let v = show_json(&db, &["--check"]);
    assert!(
        v.get("ledger_reachable").is_none(),
        "an unconfigured store was probed: {v}"
    );

    set_ledger(&db, &ledger_url());
    let v = show_json(&db, &["--check"]);
    assert!(v.get("ledger_reachable").is_some());
}

// ---------------------------------------------------------------------------
// url validation
// ---------------------------------------------------------------------------

#[test]
fn a_url_that_would_be_read_as_a_flag_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);
    qp(&db)
        .args(["remote", "set", "--ledger", "--upload-pack=x"])
        .assert()
        .failure();
    assert_eq!(show_json(&db, &[])["local_only"], Value::from(true));
}

#[test]
fn an_empty_url_is_rejected_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);
    qp(&db)
        .args(["remote", "set", "--ledger", ""])
        .assert()
        .code(1);
    assert_eq!(show_json(&db, &[])["local_only"], Value::from(true));
}

/// A ledger probe reports the object store's own diagnosis, not "unreachable".
///
/// The distinction is the whole value of the probe: absent credentials and an
/// unreachable endpoint want completely different fixes, and a follow-up
/// `qp remote doctor` can only tell apart causes this surface preserved.
#[test]
fn checking_a_ledger_reports_the_cause_rather_than_a_flat_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);

    let out = qp(&db)
        .args([
            "remote",
            "set",
            "--ledger",
            &ledger_url(),
            "--check",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a failed probe must not fail the write"
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let why = v["ledger_reachable"]["fault"]
        .as_str()
        .unwrap_or_else(|| panic!("no fault reported: {v}"));
    assert!(
        why.contains("AWS_ACCESS_KEY_ID"),
        "the probe flattened a credential problem into something unactionable: {why}"
    );
    // And the config was still written: a probe reports, it never blocks.
    assert_eq!(show_json(&db, &[])["ledger"], Value::from(ledger_url()));
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// A ledger url that is well-formed and belongs to nobody.
///
/// Config-only: `remote set` writes the url and validates its shape without
/// contacting anything, so this never resolves and never needs to.
fn ledger_url() -> String {
    "s3://minio.host.example/quipu-ledger".to_string()
}

/// A well-formed ledger url on a loopback port nothing is listening on.
///
/// Bound and released, so the port is real and free rather than guessed. This
/// is how a genuine "unreachable" is produced without leaving the machine.
fn dead_url() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    format!("s3://127.0.0.1:{port}/quipu-ledger")
}

/// Everything sitting beside the store file, so a stray artifact is visible.
fn siblings_of(db: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(db.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

fn init(db: &std::path::Path) {
    qp(db).arg("init").assert().success();
}

fn set_ledger(db: &std::path::Path, url: &str) {
    qp(db)
        .args(["remote", "set", "--ledger", url])
        .assert()
        .success();
}

fn show_json(db: &std::path::Path, extra: &[&str]) -> Value {
    let out = qp(db)
        .args(["remote", "show", "--json"])
        .args(extra)
        .output()
        .unwrap();
    assert!(out.status.success(), "remote show failed");
    serde_json::from_slice(&out.stdout).unwrap()
}

/// `qp` with the ambient store scrubbed out, so an exported `QP_DB` in the
/// environment that ran `cargo test` cannot redirect a test at a real store.
fn qp(db: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env_remove("QP_DB");
    c.env("QP_DB", db);
    // The ledger probe reads credentials from the environment, so a developer
    // with real ones exported would otherwise get a different answer from CI.
    for k in [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "QUIPU_S3_PLAINTEXT",
    ] {
        c.env_remove(k);
    }
    c
}

/// `qp` with example credentials, for the probe that must get past them.
///
/// Absent credentials fail before anything is contacted, so a test about an
/// unreachable *endpoint* has to supply some. These are AWS's own published
/// example key pair: syntactically valid, and nobody's.
fn reachable_qp(db: &std::path::Path) -> Command {
    let mut c = qp(db);
    c.env("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE");
    c.env(
        "AWS_SECRET_ACCESS_KEY",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    );
    c.env("QUIPU_S3_PLAINTEXT", "1");
    c
}
