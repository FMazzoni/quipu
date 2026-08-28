//! Project identity: who is a peer of my project, and who merely shares a ledger.
//!
//! Every store here comes from its own `qp init` against a real ledger. A
//! "second machine" made by copying a store is worthless for this file: a copy
//! shares the original's `project_uuid` AND would trivially share its key, so it
//! cannot tell apart the two things these tests exist to distinguish. The join
//! path is the real one — an object store, a sync, and `qp init --from` reading
//! batches somebody else actually wrote.
//!
//! No hostname, url or account belonging to anyone appears here; the ledger is
//! an in-process S3 stub on a loopback port, and every credential is an obvious
//! fake. Nothing in this file reaches a network.

mod common;

use assert_cmd::Command;
use common::objstore::ObjectStore;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Each test gets its own stub: a shared bucket would leak one test's projects
/// into another's, and how many projects a ledger holds is what several of
/// these tests are about.
fn qp_env(c: &mut Command, store: &ObjectStore) {
    // The guard reads this; an ambient value from the developer's shell would
    // make these tests describe that machine instead of the fixture.
    c.env_remove("QUIPU_LEDGER");
    for (k, v) in store.env() {
        c.env(k, v);
    }
}

fn qp(db: &Path, store: &ObjectStore) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env("QP_DB", db);
    qp_env(&mut c, store);
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

fn json(db: &Path, store: &ObjectStore, args: &[&str]) -> Value {
    serde_json::from_str(&ok(db, store, args)).expect("json output")
}

/// One machine: its own directory, its own `qp init`, its own uuid.
struct Machine {
    db: PathBuf,
}

impl Machine {
    /// A machine that STARTS a project.
    fn started(root: &Path, name: &str, store: &ObjectStore) -> Self {
        let db = root.join(name).join("db.sqlite");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        ok(&db, store, &["init"]);
        ok(&db, store, &["remote", "set", "--ledger", &store.url()]);
        Self { db }
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

    /// This store's batches as a plain directory, the shape `qp export` writes.
    ///
    /// There is no ledger clone any more — the whole point of the object-store
    /// transport is that nothing is mirrored locally — so a test that needs
    /// batch FILES asks for them the supported way. Identical bytes to what the
    /// sync published; only the transport differs.
    fn export_to(&self, dir: &Path, store: &ObjectStore) -> PathBuf {
        ok(&self.db, store, &["export", "--out", dir.to_str().unwrap()]);
        dir.to_path_buf()
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
}

/// A store that has published one ticket, and the ledger clone holding it.
fn published_project(root: &Path, store: &ObjectStore) -> Machine {
    let a = Machine::started(root, "a", store);
    ok(&a.db, store, &["add", "written where the project started"]);
    ok(&a.db, store, &["sync"]);
    a
}

// ---------------------------------------------------------------------------
// two identities, and what each one answers
// ---------------------------------------------------------------------------

#[test]
fn two_separate_inits_are_two_different_projects() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = Machine::started(tmp.path(), "a", &store);
    let b = Machine::started(tmp.path(), "b", &store);

    assert_ne!(a.uuid(), b.uuid(), "two `qp init`s must mint two stores");
    assert_ne!(
        a.key(),
        b.key(),
        "two `qp init`s must mint two PROJECTS: pointing them at one ledger does \
         not make them peers, and before `project_key` nothing could tell them apart"
    );
}

#[test]
fn a_joined_store_shares_the_project_key_and_not_the_store_uuid() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = published_project(tmp.path(), &store);

    // The real join path: b reads batches a actually wrote, from the ledger.
    let b_db = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(b_db.parent().unwrap()).unwrap();
    let out = json(&b_db, &store, &["init", "--from", &store.url(), "--json"]);
    assert_eq!(out["identity"], "inherited");
    let b = Machine { db: b_db };

    // Both halves. Either one alone is satisfiable by a mistake: matching keys
    // alone is what copying a store gives you, and differing uuids alone is
    // what plain `qp init` gives you.
    assert_eq!(
        a.key(),
        b.key(),
        "a machine that joined a project must carry that project's key"
    );
    assert_ne!(
        a.uuid(),
        b.uuid(),
        "a machine that joined is still its own store, and its batches must not \
         collide with the ones it joined"
    );

    // And it behaves like a peer, not just like a matching row.
    ok(
        &b.db,
        &store,
        &["add", "written on the machine that joined"],
    );
    ok(&b.db, &store, &["sync"]);
    ok(&a.db, &store, &["sync"]);
    assert_eq!(
        a.titles(),
        b.titles(),
        "peers must converge on one ticket set"
    );
    assert_eq!(a.titles().len(), 2);
}

#[test]
fn restoring_keeps_both_identities() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = published_project(tmp.path(), &store);

    let d = tmp.path().join("d").join("db.sqlite");
    std::fs::create_dir_all(d.parent().unwrap()).unwrap();
    let out = json(
        &d,
        &store,
        &[
            "init",
            "--from",
            a.export_to(&tmp.path().join("dump"), &store)
                .join(a.uuid())
                .to_str()
                .unwrap(),
            "--restore",
            "--json",
        ],
    );
    assert_eq!(out["identity"], "restored");
    let restored = Machine { db: d };

    // Restore is the OTHER operation: this store IS the original, not a peer of
    // it, so unlike a join both identities carry across.
    assert_eq!(restored.uuid(), a.uuid());
    assert_eq!(restored.key(), a.key());
    assert_eq!(restored.titles(), a.titles());
}

// ---------------------------------------------------------------------------
// init declares an intent
// ---------------------------------------------------------------------------

#[test]
fn plain_init_beside_a_populated_ledger_refuses_and_names_both_choices() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = published_project(tmp.path(), &store);

    let b = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    let assert = qp(&b, &store)
        .env(
            "QUIPU_LEDGER",
            a.export_to(&tmp.path().join("dump"), &store),
        )
        .arg("init")
        .assert()
        .failure();
    let err = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        err.contains("--from"),
        "must name the joining choice: {err}"
    );
    assert!(
        err.contains("--new-project"),
        "must name the starting choice: {err}"
    );
    assert!(!b.exists(), "a refused init must not leave a store behind");
}

#[test]
fn starting_a_new_project_on_a_populated_ledger_stays_possible() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = published_project(tmp.path(), &store);

    let c = tmp.path().join("c").join("db.sqlite");
    std::fs::create_dir_all(c.parent().unwrap()).unwrap();
    let out: Value = serde_json::from_slice(
        &qp(&c, &store)
            .env(
                "QUIPU_LEDGER",
                a.export_to(&tmp.path().join("dump"), &store),
            )
            .args(["init", "--new-project", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .unwrap();
    assert_eq!(out["identity"], "minted");
    assert_ne!(
        out["project_key"].as_str().unwrap(),
        a.key(),
        "an explicitly new project must not inherit the ledger's key"
    );
}

#[test]
fn re_initialising_an_existing_store_is_still_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = published_project(tmp.path(), &store);
    let before = a.key();

    // The guard must never fire on a store that already has an identity: the
    // ledger clone sits right beside this database, and `qp init --default-tag`
    // on an existing store is a normal thing to run.
    let out = json(
        &a.db,
        &store,
        &["init", "--default-tag", "kind:bug", "--json"],
    );
    assert_eq!(out["identity"], "existing");
    assert_eq!(a.key(), before, "re-init must not re-home a store");
}

#[test]
fn a_store_cannot_be_moved_to_another_project() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = published_project(tmp.path(), &store);
    qp(&a.db, &store)
        .args(["init", "--project-key", "some-other-project"])
        .assert()
        .failure();
    assert_ne!(a.key(), "some-other-project");
}

// ---------------------------------------------------------------------------
// the era before the key
// ---------------------------------------------------------------------------

/// Rewrites every batch in `dir` back into what a pre-identity `qp` wrote:
/// format version 1, no `project_key`. The filename and event range are
/// untouched, so it stays a batch this binary will read.
fn strip_keys(dir: &Path) {
    for store in std::fs::read_dir(dir).unwrap().flatten() {
        if !store.path().is_dir() || store.file_name() == ".git" {
            continue;
        }
        for f in std::fs::read_dir(store.path()).unwrap().flatten() {
            let path = f.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
            let mut header: Value = serde_json::from_str(&lines[0]).unwrap();
            let obj = header.as_object_mut().unwrap();
            assert!(
                obj.remove("project_key").is_some(),
                "fixture is only honest if a key was really there"
            );
            obj.insert("format_version".into(), Value::from(1));
            lines[0] = serde_json::to_string(&header).unwrap();
            std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        }
    }
}

#[test]
fn a_keyless_ledger_restores_as_the_store_that_wrote_it() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = published_project(tmp.path(), &store);
    let uuid = a.uuid();
    let dump = a.export_to(&tmp.path().join("dump"), &store);
    strip_keys(&dump);

    let r = tmp.path().join("r").join("db.sqlite");
    std::fs::create_dir_all(r.parent().unwrap()).unwrap();
    let out = json(
        &r,
        &store,
        &[
            "init",
            "--from",
            dump.join(&uuid).to_str().unwrap(),
            "--restore",
            "--json",
        ],
    );
    // The documented rule: a keyless batch is not "unknown project" and not "any
    // project" — it is the store that wrote it, standing alone.
    assert_eq!(out["project_key"], format!("legacy-{uuid}"));
    assert_eq!(out["project_uuid"], uuid);
    assert_eq!(Machine { db: r }.titles(), a.titles());
}

#[test]
fn a_keyless_ledger_can_still_be_joined_and_named() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = published_project(tmp.path(), &store);
    let uuid = a.uuid();
    let dump = a.export_to(&tmp.path().join("dump"), &store);
    strip_keys(&dump);

    let b = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    let out = json(
        &b,
        &store,
        &["init", "--from", dump.to_str().unwrap(), "--json"],
    );
    assert_eq!(out["project_key"], format!("legacy-{uuid}"));
    assert_ne!(out["project_uuid"], uuid, "joining still mints a new store");
}

/// A keyless ledger is one project's, not everyone's.
///
/// The half that matters for a shared ledger: a batch with no key must merge
/// into the store whose own key is the one that batch is understood to carry,
/// and into nothing else. Treating keyless as a wildcard would reintroduce the
/// absorption bug for every ledger written before the key existed — which is to
/// say, for every ledger that already exists.
#[test]
fn a_keyless_ledger_merges_into_its_own_project_and_no_one_elses() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = published_project(tmp.path(), &store);
    let uuid = a.uuid();
    let dump = a.export_to(&tmp.path().join("dump"), &store);
    strip_keys(&dump);
    let ledger = dump.clone();

    // The store that wrote it, as a joiner sees it: the join derives
    // `legacy-<uuid>` and folds the pre-join history in under that key.
    let b_db = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(b_db.parent().unwrap()).unwrap();
    let out = json(
        &b_db,
        &store,
        &["init", "--from", ledger.to_str().unwrap(), "--json"],
    );
    assert_eq!(out["project_key"], format!("legacy-{uuid}"));
    assert_eq!(
        Machine { db: b_db }.titles(),
        a.titles(),
        "a machine that joined the keyless project was left without its history"
    );

    // An unrelated project reading the same directory gets nothing from it.
    let s_db = tmp.path().join("s").join("db.sqlite");
    std::fs::create_dir_all(s_db.parent().unwrap()).unwrap();
    ok(&s_db, &store, &["init", "--new-project"]);
    ok(&s_db, &store, &["add", "the stranger's own ticket"]);
    let merged = json(
        &s_db,
        &store,
        &[
            "import",
            "--from",
            ledger.to_str().unwrap(),
            "--merge",
            "--json",
        ],
    );
    assert_eq!(
        merged["events_new"], 0,
        "a keyless batch was treated as matching whoever asked"
    );
    assert!(
        merged["foreign_batches"].as_u64().unwrap() > 0,
        "the batches were never even seen, so this proves the wrong thing"
    );
    assert_eq!(
        Machine { db: s_db }.titles(),
        vec!["the stranger's own ticket".to_string()]
    );
}

#[test]
fn a_store_that_predates_the_key_is_healed_rather_than_bricked() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = published_project(tmp.path(), &store);
    let uuid = a.uuid();

    // Exactly what a store initialised by an older binary looks like: every row
    // present except this one. `schema_version` is left where it is on purpose,
    // because that is the state a gated backfill would never revisit.
    {
        let conn = rusqlite::Connection::open(&a.db).unwrap();
        conn.execute("DELETE FROM meta WHERE key = 'project_key'", [])
            .unwrap();
    }

    // Any command at all heals it, and nothing else is disturbed.
    let listed = ok(&a.db, &store, &["list"]);
    assert!(listed.contains("written where the project started"));
    assert_eq!(a.key(), format!("legacy-{uuid}"));
    // And the healed key is the one its already-published keyless batches mean,
    // so its own history does not become another project's.
    ok(&a.db, &store, &["add", "after the upgrade"]);
    ok(&a.db, &store, &["sync"]);
    assert_eq!(a.titles().len(), 2);
}

// ---------------------------------------------------------------------------
// joining from a url, which is all a new machine has
// ---------------------------------------------------------------------------

#[test]
fn joining_from_a_url_needs_no_clone_and_no_further_configuration() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = published_project(tmp.path(), &store);
    let url = store.url();

    // Nothing local: no clone, no `qp remote set`, nothing run by hand. This is
    // the whole state a person has when they sit down at a second machine.
    let b_db = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(b_db.parent().unwrap()).unwrap();
    let out = json(&b_db, &store, &["init", "--from", &url, "--json"]);
    assert_eq!(out["identity"], "inherited");
    let b = Machine { db: b_db };

    assert_eq!(a.key(), b.key(), "joining by url must inherit the project");
    assert_ne!(a.uuid(), b.uuid(), "and must still be its own store");

    // The join recorded where it came from, so the machine is syncable at once.
    let shown = ok(&b.db, &store, &["remote", "show"]);
    assert!(
        shown.contains(&url),
        "join must set the ledger remote: {shown}"
    );

    // And it did not leave the project's existing tickets behind. A join that
    // merely recorded the url and left the history for the first sync would
    // never receive it: sync only fetches what it has not already accounted
    // for, and a store that has accounted for nothing still has to be given the
    // history explicitly.
    assert_eq!(
        a.titles(),
        b.titles(),
        "a join must carry the project history"
    );

    ok(
        &b.db,
        &store,
        &["add", "written on the machine that joined"],
    );
    ok(&b.db, &store, &["sync"]);
    ok(&a.db, &store, &["sync"]);
    assert_eq!(a.titles(), b.titles());
    assert_eq!(a.titles().len(), 2);
}

#[test]
fn an_exported_directory_is_read_in_place_rather_than_as_a_url() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let a = published_project(tmp.path(), &store);

    // The disaster-recovery shape: a directory of batch files, no credentials,
    // no network. It has to keep working, because it is what someone has when
    // the object store is the thing that is gone.
    let dump = a.export_to(&tmp.path().join("dump"), &store);
    let b_db = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(b_db.parent().unwrap()).unwrap();
    let out = json(
        &b_db,
        &store,
        &["init", "--from", dump.to_str().unwrap(), "--json"],
    );
    assert_eq!(out["identity"], "inherited");
    assert_eq!(out["project_key"].as_str().unwrap(), a.key());
    assert_eq!(Machine { db: b_db }.titles(), a.titles());
}

#[test]
fn the_guard_fires_for_a_ledger_that_is_only_a_url() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let _a = published_project(tmp.path(), &store);
    let url = store.url();

    let b = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    let assert = qp(&b, &store)
        .env("QUIPU_LEDGER", &url)
        .arg("init")
        .assert()
        .failure();
    let err = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        err.contains(&url),
        "the refusal must name the ledger: {err}"
    );
    assert!(
        err.contains("--from") && err.contains("--new-project"),
        "{err}"
    );
    assert!(!b.exists(), "a refused init must not leave a store behind");
}

// ---------------------------------------------------------------------------
// an empty answer has two causes
// ---------------------------------------------------------------------------

/// An empty ledger and an unreadable one look identical, so the message says so.
///
/// A LIST scoped outside your credentials' policy is not refused — it returns
/// nothing, exactly like a bucket nobody has written to. Reporting only "no
/// project here" sends someone looking for a lost ledger when the real fault is
/// a key that cannot see it.
#[test]
fn an_empty_ledger_names_the_other_thing_it_could_be() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();

    let db = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let assert = qp(&db, &store)
        .args(["init", "--from", &store.url()])
        .assert()
        .failure();
    let err = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        err.contains("no quipu batches"),
        "must say the ledger is empty: {err}"
    );
    assert!(
        err.contains("AWS_ACCESS_KEY_ID"),
        "must name the other cause an empty listing has: {err}"
    );
}
