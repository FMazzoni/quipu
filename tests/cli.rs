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
