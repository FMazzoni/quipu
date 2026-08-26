//! `qp ledger` — reading every project on a shared ledger without writing locally.
//!
//! Each project here is a SEPARATE `qp init` against one bare repository, never
//! a copy: a copied store shares the original's `project_uuid` and its project
//! key, so it cannot observe the thing this command exists for — several
//! independent projects on one ledger, each numbering its tickets from 1.
//!
//! The remote is `git init --bare` in a temp dir. No hostname, url or account
//! belonging to anyone appears in this file — the repo is public.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn qp(db: &Path) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env("QP_DB", db);
    // Otherwise an ambient ledger on the developer's machine reaches the store
    // under test through `qp init`.
    c.env_remove("QUIPU_LEDGER");
    c
}

fn ok(db: &Path, args: &[&str]) -> String {
    let out = qp(db)
        .args(args)
        .assert()
        .try_success()
        .unwrap_or_else(|e| panic!("`qp {}` failed: {e}", args.join(" ")));
    String::from_utf8_lossy(&out.get_output().stdout).to_string()
}

fn bare_repo(root: &Path) -> String {
    let path = root.join("ledger.git");
    let ok = std::process::Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&path)
        .status()
        .expect("spawn git")
        .success();
    assert!(ok, "git init --bare failed");
    format!("file://{}", path.display())
}

/// One project: its own store directory, its own `qp init`, its own key.
struct Project {
    db: PathBuf,
}

impl Project {
    fn new(root: &Path, name: &str, ledger: &str) -> Self {
        let db = root.join(name).join("db.sqlite");
        ok(&db, &["init"]);
        ok(&db, &["remote", "set", "--ledger", ledger]);
        Self { db }
    }

    /// A second machine of an EXISTING project: same key, its own uuid, its own
    /// numbering starting at 1 again.
    fn joining(root: &Path, name: &str, ledger: &str, key: &str) -> Self {
        let db = root.join(name).join("db.sqlite");
        ok(&db, &["init", "--project-key", key]);
        ok(&db, &["remote", "set", "--ledger", ledger]);
        Self { db }
    }

    fn key(&self) -> String {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        conn.query_row(
            "SELECT value FROM meta WHERE key = 'project_key'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Everything a write to this store could possibly disturb.
    ///
    /// Content rather than file bytes on purpose: the store runs in WAL mode, so
    /// a write can land in `-wal` and leave the `.sqlite` file byte-identical —
    /// a byte comparison would pass through exactly the mutation this is here to
    /// catch. Reading the tables back through a fresh connection sees the WAL.
    fn fingerprint(&self) -> String {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        let mut out = String::new();
        for table in [
            "meta",
            "task",
            "event",
            "foreign_event",
            "tag",
            "dep",
            "relation",
            "assignment",
            "conflict",
            "task_origin",
            "peer_task_origin",
            "default_tag",
        ] {
            let cols: Vec<String> = {
                let mut s = conn
                    .prepare(&format!("SELECT * FROM {table} LIMIT 0"))
                    .unwrap();
                let _ = s.query([]).unwrap();
                s.column_names().iter().map(|c| c.to_string()).collect()
            };
            let list = cols
                .iter()
                .map(|c| format!("COALESCE(CAST({c} AS TEXT), '~')"))
                .collect::<Vec<_>>()
                .join(" || '|' || ");
            let sql = format!("SELECT {list} AS row FROM {table} ORDER BY row");
            let mut stmt = conn.prepare(&sql).unwrap();
            let rows: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            out.push_str(&format!("{table}: {}\n{}\n", rows.len(), rows.join("\n")));
        }
        out
    }
}

/// Three projects on one ledger, each with tickets of its own.
fn three_projects(root: &Path) -> (String, Project, Project, Project) {
    let ledger = bare_repo(root);
    let a = Project::new(root, "alpha", &ledger);
    let b = Project::new(root, "beta", &ledger);
    let c = Project::new(root, "gamma", &ledger);
    for (p, titles) in [
        (&a, ["alpha one", "alpha two"]),
        (&b, ["beta one", "beta two"]),
        (&c, ["gamma one", "gamma two"]),
    ] {
        for t in titles {
            ok(&p.db, &["add", t]);
        }
        ok(&p.db, &["sync"]);
    }
    (ledger, a, b, c)
}

fn ledger_json(db: &Path) -> Value {
    serde_json::from_str(&ok(db, &["ledger", "--json"])).unwrap()
}

/// The whole point: one store sees every project, correctly attributed.
#[test]
fn a_query_from_one_project_returns_every_project_on_the_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let (ledger, a, b, c) = three_projects(tmp.path());

    let v = ledger_json(&a.db);
    assert_eq!(
        v["source"], "ledger",
        "the answer must name where it came from"
    );
    assert_eq!(v["ledger"].as_str().unwrap(), ledger);
    assert_eq!(v["local_project_key"].as_str().unwrap(), a.key());

    let projects = v["projects"].as_array().unwrap();
    assert_eq!(
        projects.len(),
        3,
        "three separate `qp init`s, three projects"
    );

    // The project rows and the tickets under them are two separate claims, and
    // only the second one is the feature. A replay scoped to the wrong project
    // reads nothing, writes nothing and reports success, so all three projects
    // still list — each with an empty board that is indistinguishable from a
    // ledger nobody has written to. The totals are asserted first, and by
    // number, because "three projects are listed" passes in exactly that case.
    let total: usize = projects
        .iter()
        .map(|g| g["tasks"].as_array().unwrap().len())
        .sum();
    assert_eq!(
        total, 6,
        "six tickets were synced to this ledger; the view found {total}. Zero means \
         every project replayed hollow, which reads as an empty ledger rather than a bug"
    );

    for (p, want) in [
        (&a, ["alpha one", "alpha two"]),
        (&b, ["beta one", "beta two"]),
        (&c, ["gamma one", "gamma two"]),
    ] {
        let key = p.key();
        let group = projects
            .iter()
            .find(|g| g["project_key"] == key)
            .unwrap_or_else(|| panic!("project {key} missing from the global view"));
        let tasks = group["tasks"].as_array().unwrap();
        assert_eq!(
            tasks.len(),
            2,
            "project {key} should hold exactly its own two tickets, not {}",
            tasks.len()
        );
        let titles: Vec<&str> = tasks.iter().map(|t| t["title"].as_str().unwrap()).collect();
        assert_eq!(titles, want, "wrong tickets attributed to {key}");
        // Attribution survives a consumer that flattens the groups away.
        for t in group["tasks"].as_array().unwrap() {
            assert_eq!(t["project_key"], key);
            assert!(t["origin_uuid"].as_str().is_some_and(|s| !s.is_empty()));
            assert!(t["origin_task_id"].as_i64().is_some());
        }
    }

    // Only the querying store's own project is marked local.
    let local: Vec<&Value> = projects.iter().filter(|g| g["is_local"] == true).collect();
    assert_eq!(local.len(), 1);
    assert_eq!(local[0]["project_key"].as_str().unwrap(), a.key());
}

/// The hard constraint: answering must leave the local store exactly as it was.
#[test]
fn a_global_query_writes_nothing_to_the_local_store() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, a, _, _) = three_projects(tmp.path());

    let before = a.fingerprint();
    let v = ledger_json(&a.db);
    assert_eq!(
        v["projects"].as_array().unwrap().len(),
        3,
        "nothing was read"
    );
    // Twice: an idempotence bug would otherwise hide behind a single run.
    ok(&a.db, &["ledger"]);
    let after = a.fingerprint();

    assert_eq!(before, after, "`qp ledger` mutated the store it read from");
}

/// Every project numbers from 1, so the view must never render a bare `QP-1`
/// that could be any of three tickets.
#[test]
fn colliding_display_ids_across_projects_stay_distinguishable() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, a, b, c) = three_projects(tmp.path());

    let v = ledger_json(&a.db);
    let mut ones = Vec::new();
    for g in v["projects"].as_array().unwrap() {
        for t in g["tasks"].as_array().unwrap() {
            if t["display_id"] == "QP-1" {
                ones.push((
                    g["project_key"].as_str().unwrap().to_string(),
                    t["origin_uuid"].as_str().unwrap().to_string(),
                    t["title"].as_str().unwrap().to_string(),
                ));
            }
        }
    }
    assert_eq!(ones.len(), 3, "each project should hold its own QP-1");
    let keys: std::collections::BTreeSet<&String> = ones.iter().map(|(k, ..)| k).collect();
    let uuids: std::collections::BTreeSet<&String> = ones.iter().map(|(_, u, _)| u).collect();
    assert_eq!(keys.len(), 3, "three QP-1s under fewer than three projects");
    assert_eq!(
        uuids.len(),
        3,
        "three QP-1s minted by fewer than three stores"
    );

    // In prose, the group header is what disambiguates, so it has to be there.
    let text = ok(&a.db, &["ledger"]);
    for p in [&a, &b, &c] {
        assert!(
            text.contains(&format!("project {}", p.key())),
            "human output does not say which project a row belongs to:\n{text}"
        );
    }
    assert!(
        text.contains("read-only"),
        "human output must not read like a local board:\n{text}"
    );
}

/// A store with no ledger has nothing to read, and says so without touching itself.
#[test]
fn a_store_with_no_ledger_refuses_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let lonely = Project {
        db: tmp.path().join("lonely").join("db.sqlite"),
    };
    ok(&lonely.db, &["init"]);
    ok(&lonely.db, &["add", "a local ticket"]);

    let before = lonely.fingerprint();
    let out = qp(&lonely.db).arg("ledger").assert().failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(
        stderr.contains("no ledger remote"),
        "refusal does not say what is missing: {stderr}"
    );
    assert_eq!(before, lonely.fingerprint(), "a refusal wrote to the store");
}

/// The collision that survives inside a single project, not just across them.
///
/// Two machines of one project each mint a rowid 1 without ever having met, and
/// both are `QP-1` on their own board. The view must not print one row that
/// could be either — and it must print the id the ORIGIN store shows, not a
/// number the query invented while replaying.
#[test]
fn two_stores_in_one_project_that_both_minted_a_qp_1_are_disambiguated() {
    let tmp = tempfile::tempdir().unwrap();
    let (ledger, a, b, _) = three_projects(tmp.path());
    let second = Project::joining(tmp.path(), "alpha-machine-two", &ledger, &a.key());
    assert_eq!(
        second.key(),
        a.key(),
        "the second machine joined another project"
    );
    ok(&second.db, &["add", "alpha from the second machine"]);
    ok(&second.db, &["sync"]);

    // Queried from a DIFFERENT project, so nothing local can be supplying it.
    let v = ledger_json(&b.db);
    let group = v["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["project_key"] == a.key())
        .unwrap()
        .clone();
    let tasks = group["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 3, "the second machine's ticket is missing");

    let firsts: Vec<&Value> = tasks.iter().filter(|t| t["origin_task_id"] == 1).collect();
    assert_eq!(firsts.len(), 2, "two stores each minted a rowid 1");
    let shown: Vec<&str> = firsts
        .iter()
        .map(|t| t["display_id"].as_str().unwrap())
        .collect();
    assert_ne!(
        shown[0], shown[1],
        "two different tickets rendered the same id"
    );
    for t in &firsts {
        let shown = t["display_id"].as_str().unwrap();
        assert!(
            shown.starts_with("QP-1@"),
            "a ticket its own board calls QP-1 is shown as `{shown}`"
        );
    }
    assert_ne!(firsts[0]["origin_uuid"], firsts[1]["origin_uuid"]);
}
