//! `qp remote` — the config surface, and the local-only state it exists to leave and return to.
//!
//! The authority here is a bare repository created in a temp dir. That is not a
//! stand-in for a hosted remote: `git init --bare` exercises identical ref and
//! transport semantics, and it is the topology the design actually settled on —
//! claims stay on git, because the atomic ref compare-and-swap is what makes a
//! claim a claim. The ledger is an object-store url, and these are config-only
//! tests: nothing here contacts it. Every name is RFC 2606, no credential is
//! real, and no host or account belonging to anyone appears in this file, which
//! is deliberate — the repo is public.
//!
//! The load-bearing tests here are the ones about *absence*. Local-only is the
//! default and the sanctioned escape hatch from an unreachable authority, so it
//! has to stay a first-class state rather than a path that rots from disuse.

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::Value;

// ---------------------------------------------------------------------------
// round-trip
// ---------------------------------------------------------------------------

#[test]
fn set_show_round_trips_both_urls() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let authority = bare_repo(tmp.path(), "authority");
    let ledger = ledger_url();
    init(&db);

    qp(&db)
        .args([
            "remote",
            "set",
            "--authority",
            &authority,
            "--ledger",
            &ledger,
        ])
        .assert()
        .success();

    let v = show_json(&db, &[]);
    assert_eq!(v["authority"], Value::from(authority));
    assert_eq!(v["ledger"], Value::from(ledger));
    assert_eq!(v["local_only"], Value::from(false));
}

#[test]
fn a_later_set_replaces_rather_than_appends() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let first = bare_repo(tmp.path(), "first");
    let second = bare_repo(tmp.path(), "second");
    init(&db);

    set_authority(&db, &first);
    set_authority(&db, &second);

    assert_eq!(show_json(&db, &[])["authority"], Value::from(second));
}

// ---------------------------------------------------------------------------
// the two partial states — both coherent, neither implies the other
// ---------------------------------------------------------------------------

#[test]
fn authority_only_is_a_complete_configuration() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let authority = bare_repo(tmp.path(), "authority");
    init(&db);

    set_authority(&db, &authority);

    let v = show_json(&db, &[]);
    assert_eq!(v["authority"], Value::from(authority));
    assert_eq!(
        v["ledger"],
        Value::Null,
        "setting one must not invent the other"
    );
    assert_eq!(v["local_only"], Value::from(false));
}

#[test]
fn ledger_only_is_a_complete_configuration() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let ledger = ledger_url();
    init(&db);

    qp(&db)
        .args(["remote", "set", "--ledger", &ledger])
        .assert()
        .success();

    let v = show_json(&db, &[]);
    assert_eq!(v["ledger"], Value::from(ledger));
    assert_eq!(v["authority"], Value::Null);
    assert_eq!(v["local_only"], Value::from(false));
}

#[test]
fn set_with_neither_flag_is_invalid_input() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);
    qp(&db)
        .args(["remote", "set"])
        .assert()
        .code(1)
        .stderr(contains("--authority"));
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
    assert_eq!(v["authority"], Value::Null);
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
/// every state transition succeeds, and no ssh control-socket directory is
/// created, because nothing spawned a git child at all.
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
        !tmp.path().join("ssh").exists(),
        "a store with no remote must never create an ssh control-socket dir"
    );
}

/// A configured remote still leaves every non-`claim` command local.
///
/// The url here points at a path that does not exist, so anything that tried to
/// reach it would fail. Nothing does.
#[test]
fn a_configured_remote_does_not_make_ordinary_commands_reach_out() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);
    let missing = tmp.path().join("does-not-exist.git");
    set_authority(&db, missing.to_str().unwrap());

    qp(&db).args(["add", "a"]).assert().success();
    qp(&db).args(["list"]).assert().success();
    qp(&db).args(["remote", "show"]).assert().success();
    assert!(!tmp.path().join("ssh").exists());
}

// ---------------------------------------------------------------------------
// clear — the escape hatch
// ---------------------------------------------------------------------------

#[test]
fn bare_clear_returns_the_store_to_local_only() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let authority = bare_repo(tmp.path(), "authority");
    let ledger = ledger_url();
    init(&db);
    qp(&db)
        .args([
            "remote",
            "set",
            "--authority",
            &authority,
            "--ledger",
            &ledger,
        ])
        .assert()
        .success();

    qp(&db)
        .args(["remote", "clear"])
        .assert()
        .success()
        .stdout(contains("local-only"));

    let v = show_json(&db, &[]);
    assert_eq!(v["local_only"], Value::from(true));
    assert_eq!(v["authority"], Value::Null);
    assert_eq!(v["ledger"], Value::Null);
}

#[test]
fn clear_all_is_the_same_as_bare_clear() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let authority = bare_repo(tmp.path(), "authority");
    init(&db);
    set_authority(&db, &authority);

    qp(&db)
        .args(["remote", "clear", "--all"])
        .assert()
        .success();
    assert_eq!(show_json(&db, &[])["local_only"], Value::from(true));
}

#[test]
fn clearing_one_url_leaves_the_other_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let authority = bare_repo(tmp.path(), "authority");
    let ledger = ledger_url();
    init(&db);
    qp(&db)
        .args([
            "remote",
            "set",
            "--authority",
            &authority,
            "--ledger",
            &ledger,
        ])
        .assert()
        .success();

    qp(&db)
        .args(["remote", "clear", "--authority"])
        .assert()
        .success();
    let v = show_json(&db, &[]);
    assert_eq!(v["authority"], Value::Null);
    assert_eq!(v["ledger"], Value::from(ledger.clone()));
    assert_eq!(v["local_only"], Value::from(false));

    qp(&db)
        .args(["remote", "clear", "--ledger"])
        .assert()
        .success();
    assert_eq!(show_json(&db, &[])["local_only"], Value::from(true));
}

/// Clearing what is not set is a no-op, not an error.
///
/// Someone reaching for `clear` is already blocked; making them find out which
/// half was configured would be the wrong moment to be strict.
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
    let authority = bare_repo(tmp.path(), "authority");
    init(&db);

    assert_eq!(show_json(&db, &[])["local_only"], Value::from(true));
    set_authority(&db, &authority);
    assert_eq!(show_json(&db, &[])["local_only"], Value::from(false));
    qp(&db).args(["remote", "clear"]).assert().success();
    assert_eq!(show_json(&db, &[])["local_only"], Value::from(true));
    set_authority(&db, &authority);
    assert_eq!(show_json(&db, &[])["authority"], Value::from(authority));
}

// ---------------------------------------------------------------------------
// the opt-in reachability check
// ---------------------------------------------------------------------------

#[test]
fn check_reports_a_bare_repo_as_reachable() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let authority = bare_repo(tmp.path(), "authority");
    init(&db);
    set_authority(&db, &authority);

    let v = show_json(&db, &["--check"]);
    assert_eq!(v["authority_reachable"], Value::from("ok"));
}

/// An empty bare repo has no refs at all. It is still reachable, and the probe
/// must say so — this is the case a naive `--exit-code` probe gets wrong.
#[test]
fn an_empty_bare_repo_is_reachable() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let authority = bare_repo(tmp.path(), "empty");
    init(&db);

    let out = qp(&db)
        .args([
            "remote",
            "set",
            "--authority",
            &authority,
            "--check",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["authority_reachable"], Value::from("ok"));
}

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
    let missing = tmp.path().join("nowhere.git");
    let missing = missing.to_str().unwrap();

    let out = qp(&db)
        .args(["remote", "set", "--authority", missing, "--check", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a failed probe must not fail the write"
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["authority_reachable"], Value::from("unreachable"));
    assert_eq!(v["authority"], Value::from(missing));

    // And it is genuinely persisted, not merely echoed back.
    assert_eq!(show_json(&db, &[])["authority"], Value::from(missing));
}

#[test]
fn without_check_no_reachability_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);
    set_authority(&db, tmp.path().join("nowhere.git").to_str().unwrap());

    let v = show_json(&db, &[]);
    assert!(v.get("authority_reachable").is_none());
}

#[test]
fn check_only_probes_what_is_configured() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    let authority = bare_repo(tmp.path(), "authority");
    init(&db);
    set_authority(&db, &authority);

    let v = show_json(&db, &["--check"]);
    assert_eq!(v["authority_reachable"], Value::from("ok"));
    assert!(v.get("ledger_reachable").is_none());
}

// ---------------------------------------------------------------------------
// url validation
// ---------------------------------------------------------------------------

#[test]
fn a_url_that_git_would_read_as_a_flag_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    init(&db);
    qp(&db)
        .args(["remote", "set", "--authority", "--upload-pack=x"])
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
        .args(["remote", "set", "--authority", ""])
        .assert()
        .code(1);
    assert_eq!(show_json(&db, &[])["local_only"], Value::from(true));
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

/// A real bare repository in the test's temp dir.
///
/// Identical git semantics to a remote reached over ssh, and it belongs to
/// nobody: no host, no account, nothing to leak into a public repo.
fn bare_repo(root: &std::path::Path, name: &str) -> String {
    let path = root.join(format!("{name}.git"));
    let ok = std::process::Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&path)
        .status()
        .expect("spawn git")
        .success();
    assert!(ok, "git init --bare failed");
    path.to_str().unwrap().to_string()
}

fn init(db: &std::path::Path) {
    qp(db).arg("init").assert().success();
}

fn set_authority(db: &std::path::Path, url: &str) {
    qp(db)
        .args(["remote", "set", "--authority", url])
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
    ] {
        c.env_remove(k);
    }
    c
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
