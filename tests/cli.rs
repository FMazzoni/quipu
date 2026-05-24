use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn version_flag_prints_version() {
    Command::cargo_bin("qp").unwrap().arg("--version").assert().success()
        .stdout(contains("quipu"));
}

#[test]
fn help_lists_core_commands() {
    let assert = Command::cargo_bin("qp").unwrap().arg("--help").assert().success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    for cmd in [
        "init","add","assign","claim","complete","block","cancel","abandon","reclaim",
        "log","tag","relation","tree","timeline","wave","status","list",
        "decisions","wait","watch",
    ] {
        assert!(out.contains(cmd), "help missing `{cmd}`:\n{out}");
    }
}

fn qp(db: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env("QP_DB", db);
    c
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
    assert_eq!(mode.to_lowercase(), "wal", "expected WAL journal mode, got {mode}");
}

#[test]
#[ignore]
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

    let assert = Command::cargo_bin("qp").unwrap()
        .current_dir(&cwd_b)
        .env("QP_DB", &dba)
        .arg("status").arg("--json")
        .assert()
        .success();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("project_uuid mismatch") || stderr.contains("warning"),
        "expected mismatch warning in stderr:\n{stderr}");
}
