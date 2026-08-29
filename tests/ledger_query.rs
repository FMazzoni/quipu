//! `qp ledger` — reading every project on a shared ledger without writing locally.
//!
//! Each project here is a SEPARATE `qp init` against one bucket, never a copy:
//! a copied store shares the original's `project_uuid` and its project key, so
//! it cannot observe the thing this command exists for — several independent
//! projects on one ledger, each numbering its tickets from 1.
//!
//! The remote is an in-process S3 stub on a loopback port. Nothing reaches the
//! network, and no hostname, bucket or account belonging to anyone appears in
//! this file — the repo is public.
//!
//! `qp ledger` is the one command that reads ACROSS projects, so it is the one
//! that lists the whole bucket rather than a single prefix. That is deliberate
//! and is the exception, not the rule: `qp sync` never leaves its own prefix.
//!
//! It is also the file that holds the two views to each other. `qp init --from`
//! answers the same question over the same bucket by a cheaper route, and the
//! two must not disagree about what a project is — so the join listing is
//! asserted here, beside the global query, by COUNT.

mod common;

use assert_cmd::Command;
use common::objstore::ObjectStore;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Each test gets its OWN stub, never a shared one.
///
/// A shared bucket across tests running in parallel would put other tests'
/// projects into every global view — and this file's whole subject is how many
/// projects a ledger holds, so that contamination reads as a product bug.
fn qp(db: &Path, store: &ObjectStore) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env("QP_DB", db);
    // Otherwise an ambient ledger on the developer's machine reaches the store
    // under test through `qp init`.
    c.env_remove("QUIPU_LEDGER");
    for (k, v) in store.env() {
        c.env(k, v);
    }
    c
}

fn ok(db: &Path, store: &ObjectStore, args: &[&str]) -> String {
    let out = qp(db, store)
        .args(args)
        .assert()
        .try_success()
        .unwrap_or_else(|e| panic!("`qp {}` failed: {e}", args.join(" ")));
    String::from_utf8_lossy(&out.get_output().stdout).to_string()
}

/// One project: its own store directory, its own `qp init`, its own key.
struct Project {
    db: PathBuf,
}

impl Project {
    fn new(root: &Path, name: &str, store: &ObjectStore) -> Self {
        let db = root.join(name).join("db.sqlite");
        ok(&db, store, &["init"]);
        ok(&db, store, &["remote", "set", "--ledger", &store.url()]);
        Self { db }
    }

    /// A project whose ledger url names a key PREFIX inside the same bucket.
    ///
    /// Not a second project at the root: a prefix is part of the location, so
    /// this store is as isolated from the root as it would be in another
    /// bucket. From the root it shows up as one more first-level segment, which
    /// is exactly the thing a listing cannot tell apart from a project.
    fn under_prefix(root: &Path, name: &str, store: &ObjectStore, prefix: &str) -> Self {
        let db = root.join(name).join("db.sqlite");
        ok(&db, store, &["init"]);
        ok(
            &db,
            store,
            &["remote", "set", "--ledger", &store.url_with_prefix(prefix)],
        );
        Self { db }
    }

    /// A second machine of an EXISTING project: same key, its own uuid, its own
    /// numbering starting at 1 again.
    fn joining(root: &Path, name: &str, store: &ObjectStore, key: &str) -> Self {
        let db = root.join(name).join("db.sqlite");
        ok(&db, store, &["init", "--project-key", key]);
        ok(&db, store, &["remote", "set", "--ledger", &store.url()]);
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

    fn uuid(&self) -> String {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        conn.query_row(
            "SELECT value FROM meta WHERE key = 'project_uuid'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn titles(&self) -> Vec<String> {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        let mut stmt = conn
            .prepare("SELECT title FROM task ORDER BY title")
            .unwrap();
        let v = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        v
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
fn three_projects(root: &Path, store: &ObjectStore) -> (String, Project, Project, Project) {
    let ledger = store.url();
    let a = Project::new(root, "alpha", store);
    let b = Project::new(root, "beta", store);
    let c = Project::new(root, "gamma", store);
    for (p, titles) in [
        (&a, ["alpha one", "alpha two"]),
        (&b, ["beta one", "beta two"]),
        (&c, ["gamma one", "gamma two"]),
    ] {
        for t in titles {
            ok(&p.db, store, &["add", t]);
        }
        ok(&p.db, store, &["sync"]);
    }
    (ledger, a, b, c)
}

/// The stderr of a command that must not succeed.
fn fails(db: &Path, store: &ObjectStore, args: &[&str]) -> String {
    let out = qp(db, store).args(args).assert().failure();
    String::from_utf8_lossy(&out.get_output().stderr).to_string()
}

/// The project keys `qp init --from <ledger>` would offer, read off its refusal.
///
/// There is no listing subcommand: naming the candidates is something `qp init`
/// only does when it cannot choose between them, so the ambiguity refusal IS the
/// join listing, and its count is the number under test.
fn join_candidates(db: &Path, store: &ObjectStore, url: &str) -> String {
    fails(db, store, &["init", "--from", url])
}

fn ledger_json(db: &Path, store: &ObjectStore) -> Value {
    serde_json::from_str(&ok(db, store, &["ledger", "--json"])).unwrap()
}

/// The whole point: one store sees every project, correctly attributed.
#[test]
fn a_query_from_one_project_returns_every_project_on_the_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let (ledger, a, b, c) = three_projects(tmp.path(), &store);

    let v = ledger_json(&a.db, &store);
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
    let store = ObjectStore::start();
    let (_, a, _, _) = three_projects(tmp.path(), &store);

    let before = a.fingerprint();
    let v = ledger_json(&a.db, &store);
    assert_eq!(
        v["projects"].as_array().unwrap().len(),
        3,
        "nothing was read"
    );
    // Twice: an idempotence bug would otherwise hide behind a single run.
    ok(&a.db, &store, &["ledger"]);
    let after = a.fingerprint();

    assert_eq!(before, after, "`qp ledger` mutated the store it read from");
}

/// Every project numbers from 1, so the view must never render a bare `QP-1`
/// that could be any of three tickets.
#[test]
fn colliding_display_ids_across_projects_stay_distinguishable() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let (_, a, b, c) = three_projects(tmp.path(), &store);

    let v = ledger_json(&a.db, &store);
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
    let text = ok(&a.db, &store, &["ledger"]);
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
    let store = ObjectStore::start();
    let lonely = Project {
        db: tmp.path().join("lonely").join("db.sqlite"),
    };
    ok(&lonely.db, &store, &["init"]);
    ok(&lonely.db, &store, &["add", "a local ticket"]);

    let before = lonely.fingerprint();
    let out = qp(&lonely.db, &store).arg("ledger").assert().failure();
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
    let store = ObjectStore::start();
    let (ledger, a, b, _) = three_projects(tmp.path(), &store);
    let _ = &ledger;
    let second = Project::joining(tmp.path(), "alpha-machine-two", &store, &a.key());
    assert_eq!(
        second.key(),
        a.key(),
        "the second machine joined another project"
    );
    ok(
        &second.db,
        &store,
        &["add", "alpha from the second machine"],
    );
    ok(&second.db, &store, &["sync"]);

    // Queried from a DIFFERENT project, so nothing local can be supplying it.
    let v = ledger_json(&b.db, &store);
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

// ---------------------------------------------------------------------------
// what counts as a project, from the two ends that must agree
// ---------------------------------------------------------------------------

/// A key prefix is a location, and a location is not a project.
///
/// The two paths answer the same question by different means — the join listing
/// enumerates the bucket's first-level segments, the global query reads the
/// header of every batch it downloads — and they used to disagree: a bucket
/// holding a real project plus one store publishing under a key prefix offered
/// TWO candidates to join and reported ONE project. Joining the phantom gave a
/// store built from a directory holding no batches, failing later somewhere that
/// says nothing about why.
#[test]
fn a_key_prefix_in_the_bucket_is_not_a_project_in_either_view() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();

    let a = Project::new(tmp.path(), "alpha", &store);
    ok(&a.db, &store, &["add", "alpha one"]);
    ok(&a.db, &store, &["sync"]);

    let b = Project::under_prefix(tmp.path(), "beta", &store, "team");
    ok(&b.db, &store, &["add", "beta one"]);
    ok(&b.db, &store, &["sync"]);

    // The fixture is only honest if the prefixed store really did publish under
    // a segment the root can see.
    assert!(
        store.keys().iter().any(|k| k.starts_with("team/")),
        "nothing was published under the key prefix: {:?}",
        store.keys()
    );

    let v = ledger_json(&a.db, &store);
    let projects = v["projects"].as_array().unwrap();
    assert_eq!(
        projects.len(),
        1,
        "the global query counted the key prefix as a project: {projects:?}"
    );
    assert_eq!(projects[0]["project_key"], a.key());

    // One candidate, so the join is unambiguous and lands on the one project
    // the global query named. Two candidates and this refuses instead.
    let j = tmp.path().join("joiner").join("db.sqlite");
    std::fs::create_dir_all(j.parent().unwrap()).unwrap();
    let out: Value =
        serde_json::from_str(&ok(&j, &store, &["init", "--from", &store.url(), "--json"])).unwrap();
    assert_eq!(
        out["project_key"],
        a.key(),
        "the join listing offered something other than the one project on this ledger"
    );
    assert_eq!(
        Project { db: j }.titles(),
        vec!["alpha one".to_string()],
        "the join landed on a project with no history"
    );
}

/// The number, not just the names: the count is what diverged.
#[test]
fn the_join_listing_and_the_global_query_count_the_same_projects() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();

    let a = Project::new(tmp.path(), "alpha", &store);
    let c = Project::new(tmp.path(), "gamma", &store);
    for (p, title) in [(&a, "alpha one"), (&c, "gamma one")] {
        ok(&p.db, &store, &["add", title]);
        ok(&p.db, &store, &["sync"]);
    }
    let b = Project::under_prefix(tmp.path(), "beta", &store, "team");
    ok(&b.db, &store, &["add", "beta one"]);
    ok(&b.db, &store, &["sync"]);

    let j = tmp.path().join("joiner").join("db.sqlite");
    std::fs::create_dir_all(j.parent().unwrap()).unwrap();
    let refusal = join_candidates(&j, &store, &store.url());
    assert!(
        refusal.contains("holds 2 projects"),
        "the join listing did not count 2 projects:\n{refusal}"
    );
    for key in [a.key(), c.key()] {
        assert!(
            refusal.contains(&key),
            "project {key} was not offered as a candidate:\n{refusal}"
        );
    }
    assert!(
        !refusal.contains("team"),
        "the key prefix was offered as a project to join:\n{refusal}"
    );
    assert!(
        !refusal.contains(&b.key()),
        "a project living under another prefix was offered from the root:\n{refusal}"
    );

    assert_eq!(
        ledger_json(&a.db, &store)["projects"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "the two views disagree about how many projects this ledger holds"
    );
}

/// One store's keyless and keyed batches are two projects, per FILE.
///
/// The shape every store takes the first time it publishes after upgrading: the
/// batches it wrote before project identity existed carry no key, the ones it
/// writes now do. Both come from one store uuid, so a download lays them side by
/// side in ONE directory — grouping by directory would file the older half under
/// the newer project, and dropping it would lose the store's own history from
/// the only view that spans projects.
#[test]
fn a_keyless_half_and_a_keyed_half_of_one_store_are_attributed_separately() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();

    let a = Project::new(tmp.path(), "alpha", &store);
    let uuid = a.uuid();
    ok(&a.db, &store, &["add", "written before the key"]);

    // The keyless half, hand-written: `qp export` cannot produce one any more,
    // so the fixture takes a real batch and puts it back into the shape a
    // pre-identity `qp` wrote — format version 1, no `project_key`.
    let dump = tmp.path().join("dump");
    ok(&a.db, &store, &["export", "--out", dump.to_str().unwrap()]);
    let early = std::fs::read_dir(dump.join(&uuid))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .expect("an exported batch");
    let name = early.file_name().unwrap().to_str().unwrap().to_string();
    let keyless = strip_key(&std::fs::read_to_string(&early).unwrap());

    // The keyed half: a second ticket, then a real sync. Its batch closes over
    // a wider range, so the two halves cannot collide on a filename once the
    // download flattens them into one directory.
    ok(&a.db, &store, &["add", "written after the key"]);
    ok(&a.db, &store, &["sync"]);

    // Where a pre-identity store's batches resolve to: its own derived key.
    store.place(&format!("legacy-{uuid}/{uuid}/{name}"), keyless.as_bytes());

    let v = ledger_json(&a.db, &store);
    let projects = v["projects"].as_array().unwrap();
    assert_eq!(
        projects.len(),
        2,
        "the keyless half was dropped or merged into the keyed one: {projects:?}"
    );

    let legacy = projects
        .iter()
        .find(|g| g["project_key"] == format!("legacy-{uuid}"))
        .expect("the keyless half is its own project, under the derived key");
    let titles: Vec<&str> = legacy["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        vec!["written before the key"],
        "the keyless half replayed as something other than the history it holds"
    );

    let keyed = projects
        .iter()
        .find(|g| g["project_key"] == a.key())
        .expect("the keyed half is the store's current project");
    let mut titles: Vec<&str> = keyed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    titles.sort_unstable();
    assert_eq!(
        titles,
        vec!["written after the key", "written before the key"],
        "the keyed half lost history the store had published"
    );
}

/// One batch, put back into the shape a `qp` that predates the key wrote.
fn strip_key(batch: &str) -> String {
    let mut lines: Vec<String> = batch.lines().map(str::to_string).collect();
    let mut header: Value = serde_json::from_str(&lines[0]).unwrap();
    let obj = header.as_object_mut().unwrap();
    assert!(
        obj.remove("project_key").is_some(),
        "fixture is only honest if a key was really there"
    );
    obj.insert("format_version".into(), Value::from(1));
    lines[0] = serde_json::to_string(&header).unwrap();
    lines.join("\n") + "\n"
}
