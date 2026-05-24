use assert_cmd::Command;
fn qp(db: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env("QP_DB", db);
    c
}

#[test]
fn wave_pattern_with_tags_and_critique() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db.sqlite");
    qp(&db).arg("init").assert().success();

    for title in ["X", "Y", "Z"] {
        qp(&db).args(["add", title, "--tier", "wave-1", "--tag", "wave:1"]).assert().success();
    }
    for (i, t) in [("QP-1","a1"),("QP-2","a2"),("QP-3","a3")] {
        let _ = (i,t);
        qp(&db).args(["assign", i, "--to", t]).assert().success();
        qp(&db).args(["claim", i, "--as", t]).assert().success();
        qp(&db).args(["complete", i, "--as", t, "--decision", &format!("did {i}")]).assert().success();
    }
    let out = qp(&db).args(["status","--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout).unwrap().trim()).unwrap();
    assert_eq!(v["done"], 3);

    // Critique against QP-2 → new task tagged kind:critique.
    let out = qp(&db).args(["add","fix: missed timeout","--tag","wave:1","--tag","kind:critique","--depends-on","QP-2","--json"])
        .assert().success();
    let crit: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout).unwrap().trim()).unwrap();
    // Since QP-2 is done, the critique starts ready (dep already done).
    assert_eq!(crit["state"], "ready");
    let cid = crit["display_id"].as_str().unwrap().to_string();

    qp(&db).args(["assign", &cid, "--to", "fix-1"]).assert().success();
    qp(&db).args(["claim",  &cid, "--as", "fix-1"]).assert().success();
    qp(&db).args(["complete", &cid, "--as", "fix-1", "--decision", "fixed"]).assert().success();

    // Branch-and-evaluate slice.
    let root_out = qp(&db).args(["add","explore alt","--tag","wave:1","--json"]).assert().success();
    let root: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&root_out.get_output().stdout).unwrap().trim()).unwrap();
    let root_id = root["display_id"].as_str().unwrap().to_string();
    let mut variants = Vec::new();
    for v in ["a", "b", "c"] {
        let o = qp(&db).args(["add", &format!("try-{v}"), "--tag", "wave:1", "--json"]).assert().success();
        let j: serde_json::Value = serde_json::from_str(
            std::str::from_utf8(&o.get_output().stdout).unwrap().trim()).unwrap();
        let id = j["display_id"].as_str().unwrap().to_string();
        qp(&db).args(["relation","add", &id, "variant-of", &root_id]).assert().success();
        variants.push(id);
    }
    // Cancel losers (keep first variant as winner).
    let winner = variants[0].clone();
    for loser in &variants[1..] { qp(&db).args(["cancel", loser, "--reason", "superseded"]).assert().success(); }
    qp(&db).args(["relation","add", &winner, "supersedes", &root_id]).assert().success();

    // List filter by tag works.
    let out = qp(&db).args(["list","--tag","wave:1","--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout).unwrap().trim()).unwrap();
    assert!(v.as_array().unwrap().len() >= 7);

    // Decisions: at least the four we logged.
    let out = qp(&db).args(["decisions","--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout).unwrap().trim()).unwrap();
    assert!(v.as_array().unwrap().len() >= 4);

    // Wave should now have empty `running` (everything done/cancelled).
    let out = qp(&db).args(["wave","--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&out.get_output().stdout).unwrap().trim()).unwrap();
    assert!(v["running"].as_array().unwrap().is_empty());

    // Skill file is present and references the commands we use.
    let skill = std::fs::read_to_string("skills/wave/SKILL.md").expect("skill present");
    for cmd in ["qp init","qp add","qp assign","qp claim","qp complete",
                "qp wait","qp wave","qp list","qp timeline","qp decisions",
                "qp cancel","qp relation add","qp reclaim"] {
        assert!(skill.contains(cmd), "skill missing `{cmd}`");
    }
}
