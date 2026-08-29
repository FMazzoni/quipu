//! `qp sync` — a real two-machine round trip over an object store.
//!
//! Every "second machine" here is a SEPARATE `qp init`, never a copy of the
//! first. That is the whole point: a copied store shares the original's
//! `project_uuid` and mints the same display ids, so it cannot observe the
//! bugs that only appear between genuinely distinct peers — colliding `QP-1`s,
//! a batch key that is not unique, an origin map that maps nothing.
//!
//! The remote is an in-process S3 stub on a loopback port (`common::objstore`),
//! and it is not a filesystem fake: every request here is a real socket
//! carrying a real signed url, so the transport is under test alongside the
//! exchange. Nothing reaches the network, no credential in this file is real,
//! and no hostname, bucket or account belonging to anyone appears in it — the
//! repo is public.

mod common;

use assert_cmd::Command;
use common::objstore::ObjectStore;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

fn qp(db: &Path, store: &ObjectStore) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env("QP_DB", db);
    for (k, v) in store.env() {
        c.env(k, v);
    }
    c
}

/// A machine: its own store directory, its own `qp init`, its own uuid.
struct Machine<'a> {
    db: PathBuf,
    store: &'a ObjectStore,
}

impl<'a> Machine<'a> {
    /// A machine that STARTS a project: its own uuid and its own fresh key.
    fn new(root: &Path, name: &str, store: &'a ObjectStore) -> Self {
        Self::init(root, name, store, &["init"])
    }

    /// A machine that JOINS the project `key` — a second machine of one project.
    ///
    /// Still its own `qp init`, never a copy: a copied store would share the
    /// original's uuid, mint the same display ids and write to the same object
    /// keys, so it could not observe anything a real second machine can. What
    /// it does not do is mint a second project, which is what plain `qp init`
    /// does and what makes two stores strangers that merely share a ledger.
    fn joining(root: &Path, name: &str, store: &'a ObjectStore, key: &str) -> Self {
        Self::init(root, name, store, &["init", "--project-key", key])
    }

    fn init(root: &Path, name: &str, store: &'a ObjectStore, args: &[&str]) -> Self {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let m = Self {
            db: dir.join("db.sqlite"),
            store,
        };
        m.ok(args);
        m.ok(&["remote", "set", "--ledger", &store.url()]);
        m
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = qp(&self.db, self.store)
            .args(args)
            .assert()
            .try_success()
            .unwrap_or_else(|e| panic!("`qp {}` failed: {e}", args.join(" ")));
        String::from_utf8_lossy(&out.get_output().stdout).to_string()
    }

    fn meta(&self, key: &str) -> String {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .unwrap_or_else(|e| panic!("meta `{key}`: {e}"))
    }

    fn uuid(&self) -> String {
        self.meta("project_uuid")
    }

    fn key(&self) -> String {
        self.meta("project_key")
    }

    fn sync(&self) -> Value {
        serde_json::from_str(&self.ok(&["sync", "--json"])).unwrap()
    }

    /// Every task title in the store, sorted. The observable that has to survive a sync.
    fn titles(&self) -> Vec<String> {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        let mut stmt = conn
            .prepare("SELECT title FROM task ORDER BY title")
            .unwrap();
        let v: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v
    }

    /// A fingerprint of everything a sync could disturb.
    fn snapshot(&self) -> String {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        let mut out = String::new();
        for sql in [
            "SELECT display_id, title, state FROM task ORDER BY id",
            "SELECT task_id, name FROM tag ORDER BY task_id, name",
            "SELECT id, kind, ts FROM event ORDER BY id",
            "SELECT origin_uuid, origin_id, kind FROM foreign_event ORDER BY origin_uuid, origin_id",
            "SELECT key FROM ledger_object ORDER BY key",
        ] {
            let mut stmt = conn.prepare(sql).unwrap();
            let cols = stmt.column_count();
            let rows: Vec<String> = stmt
                .query_map([], |r| {
                    let mut cells = Vec::new();
                    for i in 0..cols {
                        cells.push(format!("{:?}", r.get::<_, rusqlite::types::Value>(i)?));
                    }
                    Ok(cells.join("|"))
                })
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            out.push_str(&rows.join("\n"));
            out.push('\n');
        }
        out
    }
}

/// Object keys under one project's prefix, which is the only prefix its stores read.
fn keys_for(store: &ObjectStore, project_key: &str) -> Vec<String> {
    let prefix = format!("{project_key}/");
    store
        .keys()
        .into_iter()
        .filter(|k| k.starts_with(&prefix))
        .collect()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ---------------------------------------------------------------------------
// the round trip
// ---------------------------------------------------------------------------

#[test]
fn two_machines_exchange_their_tickets_through_the_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = Machine::new(tmp.path(), "a", &store);
    let b = Machine::joining(tmp.path(), "b", &store, &a.key());

    // Genuinely distinct peers, which is what makes the rest of this test mean
    // anything. A copied store would fail here.
    assert_ne!(a.uuid(), b.uuid(), "two `qp init`s must mint two uuids");
    assert_eq!(a.key(), b.key(), "these two are meant to be one project");

    a.ok(&["add", "written on machine a"]);
    a.ok(&["add", "also on machine a"]);
    b.ok(&["add", "written on machine b"]);

    // Both minted QP-1: the collision a same-uuid fake could never produce.
    assert_eq!(a.titles().len(), 2);
    assert_eq!(b.titles().len(), 1);

    a.sync(); // publishes a
    b.sync(); // reads a, publishes b
    a.sync(); // reads b

    let want = vec![
        "also on machine a".to_string(),
        "written on machine a".to_string(),
        "written on machine b".to_string(),
    ];
    assert_eq!(a.titles(), want, "machine a is missing b's ticket");
    assert_eq!(b.titles(), want, "machine b is missing a's tickets");

    // Every object landed under `<project key>/<store uuid>/`, which is the
    // layout the prefix-scoped read depends on.
    let keys = keys_for(&store, &a.key());
    let mine = format!("{}/{}/", a.key(), a.uuid());
    let theirs = format!("{}/{}/", a.key(), b.uuid());
    assert!(
        keys.iter().any(|k| k.starts_with(&mine)) && keys.iter().any(|k| k.starts_with(&theirs)),
        "objects are not laid out project-first: {keys:?}"
    );
}

#[test]
fn a_second_sync_with_nothing_new_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = Machine::new(tmp.path(), "a", &store);
    let b = Machine::joining(tmp.path(), "b", &store, &a.key());

    a.ok(&["add", "from a"]);
    b.ok(&["add", "from b"]);
    a.sync();
    b.sync();
    a.sync();

    let before = a.snapshot();
    let objects_before = store.snapshot();

    let out = a.sync();
    assert_eq!(out["pushed"], Value::from(false), "nothing to publish");
    assert_eq!(out["put"], Value::Null, "an idle sync wrote an object");
    assert_eq!(out["fetched"], Value::Array(vec![]), "nothing to fetch");
    assert_eq!(out["import"], Value::Null, "no merge should have run");
    assert_eq!(out["export"]["event_count"], Value::from(0));
    assert_eq!(out["export"]["file"], Value::Null);

    assert_eq!(a.snapshot(), before, "an idle sync mutated the store");
    assert_eq!(
        store.snapshot(),
        objects_before,
        "an idle sync wrote to the ledger"
    );
}

/// The scaling claim, asserted at the wire rather than argued.
#[test]
fn a_sync_reads_only_its_own_projects_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = Machine::new(tmp.path(), "a", &store);
    let stranger = Machine::new(tmp.path(), "stranger", &store);
    assert_ne!(a.key(), stranger.key());

    a.ok(&["add", "mine"]);
    stranger.ok(&["add", "theirs"]);
    a.sync();
    stranger.sync();
    assert!(
        !keys_for(&store, &stranger.key()).is_empty(),
        "the stranger published nothing, so this proves the wrong thing"
    );

    store.forget_requests();
    a.ok(&["add", "more of mine"]);
    a.sync();

    // Nothing this store requested names another project. A git fetch could not
    // make this promise — it takes the branch whole — and that is the reason
    // the transport changed.
    let foreign = format!("{}/", stranger.key());
    let seen: Vec<String> = store
        .requests()
        .iter()
        .map(|r| format!("{} {}", r.path(), r.query()))
        .collect();
    assert!(!seen.is_empty(), "the sync issued no requests at all");
    for line in &seen {
        assert!(
            !line.contains(&foreign),
            "a sync reached for another project's data: {line}"
        );
    }
}

#[test]
fn sync_never_rewrites_an_object_another_store_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = Machine::new(tmp.path(), "a", &store);
    let b = Machine::joining(tmp.path(), "b", &store, &a.key());

    a.ok(&["add", "a first"]);
    b.ok(&["add", "b first"]);
    a.sync();
    b.sync();
    a.sync();

    let peers_before: BTreeMap<String, Vec<u8>> = store
        .snapshot()
        .into_iter()
        .filter(|(k, _)| k.contains(&b.uuid()))
        .collect();
    assert!(!peers_before.is_empty(), "b never published a batch");

    // More rounds, so there is something for both sides to write.
    b.ok(&["add", "b second"]);
    a.ok(&["add", "a second"]);
    b.sync();
    a.sync();

    let after = store.snapshot();
    for (key, bytes) in &peers_before {
        assert_eq!(
            after.get(key),
            Some(bytes),
            "{key} was rewritten or removed by a later sync"
        );
    }
}

#[test]
fn a_store_with_no_ledger_remote_fails_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let dir = tmp.path().join("solo");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("db.sqlite");
    qp(&db, &store).arg("init").assert().success();
    qp(&db, &store)
        .args(["add", "local only"])
        .assert()
        .success();

    let conn = rusqlite::Connection::open(&db).unwrap();
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))
        .unwrap();

    let out = qp(&db, &store).arg("sync").assert().failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(
        stderr.contains("no ledger remote") && stderr.contains("qp remote set --ledger"),
        "unhelpful failure: {stderr}"
    );

    assert!(store.keys().is_empty(), "a failed sync wrote to the ledger");
    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after, "a failed sync wrote to the store");
}

#[test]
fn a_sync_leaves_nothing_for_the_project_repo_to_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();

    // A project repo shaped like a real one: a store under `.quipu/`, which the
    // project already ignores. The owner's requirement is that a sync must never
    // leave a file a person could accidentally commit — and with the ledger in
    // object storage there is no clone to leave anywhere at all.
    let project = tmp.path().join("project");
    std::fs::create_dir_all(project.join(".quipu")).unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(&project)
        .status()
        .unwrap();
    std::fs::write(project.join(".gitignore"), "/.quipu\n").unwrap();
    git(&project, &["add", "-A"]);
    git(
        &project,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "seed",
        ],
    );

    let db = project.join(".quipu").join("db.sqlite");
    let m = Machine {
        db: db.clone(),
        store: &store,
    };
    m.ok(&["init"]);
    m.ok(&["remote", "set", "--ledger", &store.url()]);
    m.ok(&["add", "work"]);

    let peer = Machine::joining(tmp.path(), "peer", &store, &m.key());
    peer.ok(&["add", "peer work"]);
    peer.sync();

    m.sync();

    assert!(
        !db.parent().unwrap().join("ledger").exists(),
        "a sync left a ledger clone behind; the object-store ledger has none"
    );
    assert_eq!(
        git(&project, &["status", "--porcelain"]),
        "",
        "a sync left something the project repo would commit"
    );
}

// ---------------------------------------------------------------------------
// one ledger, several projects
// ---------------------------------------------------------------------------

/// Three stores on one ledger: two machines of one project, and a stranger.
///
/// Both halves of the property in one fixture on purpose. Scoping alone is
/// satisfied by a filter that merges nothing, and convergence alone is satisfied
/// by the absorbing merge this scoping exists to stop; only a fixture that
/// demands both at once can tell a correct filter from either mistake.
#[test]
fn one_ledger_holds_several_projects_without_mixing_them() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a1 = Machine::new(tmp.path(), "a1", &store);
    let a2 = Machine::joining(tmp.path(), "a2", &store, &a1.key());
    let b = Machine::new(tmp.path(), "b", &store);

    assert_eq!(a1.key(), a2.key(), "a1 and a2 are one project");
    assert_ne!(a1.key(), b.key(), "b is a different project");
    assert_ne!(a1.uuid(), a2.uuid(), "peers are separate stores");

    a1.ok(&["add", "first machine of the first project"]);
    a2.ok(&["add", "second machine of the first project"]);
    b.ok(&["add", "the other project entirely"]);

    // Two rounds each, so everyone has both published and read.
    for _ in 0..2 {
        a1.sync();
        a2.sync();
        b.sync();
    }
    a1.sync();

    let project_a = vec![
        "first machine of the first project".to_string(),
        "second machine of the first project".to_string(),
    ];
    assert_eq!(a1.titles(), project_a, "a1 did not converge with its peer");
    assert_eq!(a2.titles(), project_a, "a2 did not converge with its peer");
    assert_eq!(
        b.titles(),
        vec!["the other project entirely".to_string()],
        "b absorbed another project's tickets"
    );

    // The stranger's batches ARE on the ledger — the bucket is shared, and a
    // global read across projects depends on that. What changed is that nobody
    // has to download them in order to ignore them.
    assert!(
        !keys_for(&store, &b.key()).is_empty(),
        "the stranger published nothing, so this proves the wrong thing"
    );

    // The merge itself, aimed at a directory holding BOTH projects the way a
    // careless caller might. Asserted separately because sync also declines to
    // call it when nothing of ours arrived: with only that check in place, an
    // unscoped merge would still absorb the ledger the moment anything else
    // invoked one.
    let mixed = tmp.path().join("mixed");
    for (key, bytes) in store.snapshot() {
        let mut parts = key.split('/');
        parts.next();
        let uuid = parts.next().unwrap().to_string();
        let file = parts.next().unwrap().to_string();
        std::fs::create_dir_all(mixed.join(&uuid)).unwrap();
        std::fs::write(mixed.join(&uuid).join(&file), bytes).unwrap();
    }
    let out = a1.ok(&[
        "import",
        "--from",
        mixed.to_str().unwrap(),
        "--merge",
        "--json",
    ]);
    let out: Value = serde_json::from_str(&out).unwrap();
    assert!(
        out["foreign_batches"].as_u64().unwrap() > 0,
        "the merge saw no other project at all, so this proves the wrong thing"
    );
    assert_eq!(
        a1.titles(),
        project_a,
        "a merge pointed at a mixed ledger absorbed another project"
    );
}

/// The stranger's arrival must not read as "something new for me".
#[test]
fn another_projects_batches_are_not_something_this_store_has_to_merge() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = Machine::new(tmp.path(), "a", &store);
    let b = Machine::new(tmp.path(), "b", &store);

    a.ok(&["add", "mine"]);
    a.sync();
    b.ok(&["add", "theirs"]);
    b.sync();

    let before = a.snapshot();
    let out = a.sync();
    assert_eq!(
        out["fetched"],
        Value::Array(vec![]),
        "another project's batch counted as a peer batch"
    );
    assert_eq!(
        out["import"],
        Value::Null,
        "a merge ran with nothing to merge"
    );
    assert_eq!(
        a.snapshot(),
        before,
        "a stranger's publish disturbed this store"
    );

    // And it really is on the ledger — otherwise the assertions above would
    // hold for a store facing an empty bucket.
    assert!(
        !keys_for(&store, &b.key()).is_empty(),
        "b's batch never reached the ledger"
    );
}

/// A store joining later still receives everything written before it arrived.
#[test]
fn a_store_that_joins_late_receives_the_history_it_missed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = Machine::new(tmp.path(), "a", &store);
    a.ok(&["add", "written long before b existed"]);
    a.sync();

    let dir = tmp.path().join("b");
    std::fs::create_dir_all(&dir).unwrap();
    let b = Machine {
        db: dir.join("db.sqlite"),
        store: &store,
    };
    b.ok(&["init", "--from", &store.url()]);

    assert_eq!(b.key(), a.key(), "the join did not adopt a's project");
    assert_ne!(b.uuid(), a.uuid(), "a join must not copy the store's uuid");
    assert_eq!(
        b.titles(),
        vec!["written long before b existed".to_string()],
        "a store joining from a url did not receive the pre-join history"
    );

    // And the join recorded what it read, so the first sync is not a full
    // re-download of the project it just folded in.
    let out = b.sync();
    assert_eq!(
        out["fetched"],
        Value::Array(vec![]),
        "the first sync after a join re-fetched the history the join already read"
    );
}

/// A keyless batch belongs to the store that wrote it, never to whoever asks.
///
/// A store that predates project identity exports batches with no `project_key`,
/// and its effective key is derived from its own uuid. A different project must
/// not absorb it, and it must not absorb a different project.
#[test]
fn a_keyless_batch_belongs_to_the_store_that_wrote_it() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = Machine::new(tmp.path(), "a", &store);
    a.ok(&["add", "mine"]);
    a.sync();

    // A batch as a pre-identity store would have written it: same shape, no
    // `project_key` in the header, sitting under the derived prefix that the
    // store's own key would resolve to.
    let (key, bytes) = store
        .snapshot()
        .into_iter()
        .next()
        .expect("a published nothing");
    let text = String::from_utf8(bytes).unwrap();
    let mut lines = text.lines();
    let mut header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let stranger_uuid = "00000000-0000-4000-8000-00000000dead";
    header["project_uuid"] = Value::from(stranger_uuid);
    header.as_object_mut().unwrap().remove("project_key");
    let file = key
        .rsplit('/')
        .next()
        .unwrap()
        .replace(key.split('/').nth(1).unwrap(), stranger_uuid);
    let mut rebuilt = serde_json::to_string(&header).unwrap();
    for l in lines {
        rebuilt.push('\n');
        rebuilt.push_str(l);
    }

    // Filed under a's project prefix, which is the hostile placement: if the
    // prefix decided ownership rather than the header, a would swallow it.
    let planted = format!("{}/{stranger_uuid}/{file}", a.key());
    let before = a.titles();
    let dir = tmp.path().join("planted").join(stranger_uuid);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(&file), &rebuilt).unwrap();
    let out = a.ok(&[
        "import",
        "--from",
        tmp.path().join("planted").to_str().unwrap(),
        "--merge",
        "--json",
    ]);
    let out: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        out["foreign_batches"].as_u64().unwrap(),
        1,
        "a keyless batch from another store was treated as this project's"
    );
    assert_eq!(a.titles(), before, "a keyless batch was absorbed");
    assert!(!planted.is_empty());
}
