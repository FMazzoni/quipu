//! Project identity: who is a peer of my project, and who merely shares a ledger.
//!
//! Every store here comes from its own `qp init` against a real ledger. A
//! "second machine" made by copying a store is worthless for this file: a copy
//! shares the original's `project_uuid` AND would trivially share its key, so it
//! cannot tell apart the two things these tests exist to distinguish. The join
//! path is the real one — a bare git repository, a sync, and `qp init --from`
//! reading batches somebody else actually wrote.
//!
//! No hostname, url or account belonging to anyone appears here; the ledger is
//! `git init --bare` in a temp directory, reached by path or by a `file://` url.
//! Nothing in this file touches a network.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn qp(db: &Path) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env("QP_DB", db);
    // The guard reads this; an ambient value from the developer's shell would
    // make these tests describe that machine instead of the fixture.
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

fn json(db: &Path, args: &[&str]) -> Value {
    serde_json::from_str(&ok(db, args)).expect("json output")
}

fn bare_repo(root: &Path) -> PathBuf {
    let path = root.join("ledger.git");
    assert!(std::process::Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&path)
        .status()
        .expect("spawn git")
        .success());
    path
}

/// One machine: its own directory, its own `qp init`, its own uuid.
struct Machine {
    db: PathBuf,
}

impl Machine {
    /// A machine that STARTS a project.
    fn started(root: &Path, name: &str, remote: &Path) -> Self {
        let db = root.join(name).join("db.sqlite");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        ok(&db, &["init"]);
        ok(
            &db,
            &["remote", "set", "--ledger", remote.to_str().unwrap()],
        );
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

    fn ledger_clone(&self) -> PathBuf {
        self.db.parent().unwrap().join("ledger")
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

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn git");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A store that has published one ticket, and the ledger clone holding it.
fn published_project(root: &Path) -> (Machine, PathBuf) {
    let remote = bare_repo(root);
    let a = Machine::started(root, "a", &remote);
    ok(&a.db, &["add", "written where the project started"]);
    ok(&a.db, &["sync"]);
    (a, remote)
}

// ---------------------------------------------------------------------------
// two identities, and what each one answers
// ---------------------------------------------------------------------------

#[test]
fn two_separate_inits_are_two_different_projects() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = bare_repo(tmp.path());
    let a = Machine::started(tmp.path(), "a", &remote);
    let b = Machine::started(tmp.path(), "b", &remote);

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
    let (a, remote) = published_project(tmp.path());

    // The real join path: b reads batches a actually wrote, through a's clone.
    let b_db = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(b_db.parent().unwrap()).unwrap();
    let out = json(
        &b_db,
        &[
            "init",
            "--from",
            a.ledger_clone().to_str().unwrap(),
            "--json",
        ],
    );
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
        &["remote", "set", "--ledger", remote.to_str().unwrap()],
    );
    ok(&b.db, &["add", "written on the machine that joined"]);
    ok(&b.db, &["sync"]);
    ok(&a.db, &["sync"]);
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
    let (a, _remote) = published_project(tmp.path());

    let d = tmp.path().join("d").join("db.sqlite");
    std::fs::create_dir_all(d.parent().unwrap()).unwrap();
    let out = json(
        &d,
        &[
            "init",
            "--from",
            a.ledger_clone().join(a.uuid()).to_str().unwrap(),
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
    let (a, _remote) = published_project(tmp.path());

    let b = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    let assert = qp(&b)
        .env("QUIPU_LEDGER", a.ledger_clone())
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
    let (a, _remote) = published_project(tmp.path());

    let c = tmp.path().join("c").join("db.sqlite");
    std::fs::create_dir_all(c.parent().unwrap()).unwrap();
    let out: Value = serde_json::from_slice(
        &qp(&c)
            .env("QUIPU_LEDGER", a.ledger_clone())
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
    let (a, _remote) = published_project(tmp.path());
    let before = a.key();

    // The guard must never fire on a store that already has an identity: the
    // ledger clone sits right beside this database, and `qp init --default-tag`
    // on an existing store is a normal thing to run.
    let out = json(&a.db, &["init", "--default-tag", "kind:bug", "--json"]);
    assert_eq!(out["identity"], "existing");
    assert_eq!(a.key(), before, "re-init must not re-home a store");
}

#[test]
fn a_store_cannot_be_moved_to_another_project() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, _remote) = published_project(tmp.path());
    qp(&a.db)
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
    let (a, _remote) = published_project(tmp.path());
    let uuid = a.uuid();
    strip_keys(&a.ledger_clone());

    let r = tmp.path().join("r").join("db.sqlite");
    std::fs::create_dir_all(r.parent().unwrap()).unwrap();
    let out = json(
        &r,
        &[
            "init",
            "--from",
            a.ledger_clone().join(&uuid).to_str().unwrap(),
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
    let (a, _remote) = published_project(tmp.path());
    let uuid = a.uuid();
    strip_keys(&a.ledger_clone());

    let b = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    let out = json(
        &b,
        &[
            "init",
            "--from",
            a.ledger_clone().to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out["project_key"], format!("legacy-{uuid}"));
    assert_ne!(out["project_uuid"], uuid, "joining still mints a new store");
}

#[test]
fn a_store_that_predates_the_key_is_healed_rather_than_bricked() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, _remote) = published_project(tmp.path());
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
    let listed = ok(&a.db, &["list"]);
    assert!(listed.contains("written where the project started"));
    assert_eq!(a.key(), format!("legacy-{uuid}"));
    // And the healed key is the one its already-published keyless batches mean,
    // so its own history does not become another project's.
    ok(&a.db, &["add", "after the upgrade"]);
    ok(&a.db, &["sync"]);
    assert_eq!(a.titles().len(), 2);
}

// ---------------------------------------------------------------------------
// joining from a url, which is all a new machine has
// ---------------------------------------------------------------------------

#[test]
fn joining_from_a_url_needs_no_clone_and_no_further_configuration() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, remote) = published_project(tmp.path());
    let url = format!("file://{}", remote.display());

    // Nothing local: no clone, no `qp remote set`, no `git` run by hand. This is
    // the whole state a person has when they sit down at a second machine.
    let b_db = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(b_db.parent().unwrap()).unwrap();
    let out = json(&b_db, &["init", "--from", &url, "--json"]);
    assert_eq!(out["identity"], "inherited");
    let b = Machine { db: b_db };

    assert_eq!(a.key(), b.key(), "joining by url must inherit the project");
    assert_ne!(a.uuid(), b.uuid(), "and must still be its own store");

    // The join recorded where it came from, so the machine is syncable at once.
    let shown = ok(&b.db, &["remote", "show"]);
    assert!(
        shown.contains(&url),
        "join must set the ledger remote: {shown}"
    );

    // And it did not leave the project's existing tickets behind: sync decides
    // what is new by diffing its clone across a fetch, so a join that populated
    // that clone and left it at that would never receive them at all.
    assert_eq!(
        a.titles(),
        b.titles(),
        "a join must carry the project history"
    );

    ok(&b.db, &["add", "written on the machine that joined"]);
    ok(&b.db, &["sync"]);
    ok(&a.db, &["sync"]);
    assert_eq!(a.titles(), b.titles());
    assert_eq!(a.titles().len(), 2);
}

#[test]
fn a_bare_ledger_path_is_read_as_a_remote_not_as_a_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, remote) = published_project(tmp.path());

    // A bare repository is BOTH an existing directory and a git url, and its
    // batches are in `objects` rather than in a tree. Reading it as a directory
    // finds nothing and reports an empty ledger about a full one.
    let b_db = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(b_db.parent().unwrap()).unwrap();
    let out = json(
        &b_db,
        &["init", "--from", remote.to_str().unwrap(), "--json"],
    );
    assert_eq!(out["identity"], "inherited");
    assert_eq!(out["project_key"].as_str().unwrap(), a.key());
    assert_eq!(Machine { db: b_db }.titles(), a.titles());
}

#[test]
fn the_guard_fires_for_a_ledger_that_is_only_a_url() {
    let tmp = tempfile::tempdir().unwrap();
    let (_a, remote) = published_project(tmp.path());
    let url = format!("file://{}", remote.display());

    let b = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    let assert = qp(&b)
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
// the branch the batches live on
// ---------------------------------------------------------------------------

#[test]
fn a_plain_clone_of_a_published_ledger_lands_on_the_batches() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, remote) = published_project(tmp.path());

    // A bare repository's HEAD names `main`/`master`, and pushing a branch does
    // NOT move it — so without quipu adopting it, this clone is an empty tree
    // and every downstream message describes a full ledger as an empty one.
    let clone = tmp.path().join("by-hand");
    assert!(std::process::Command::new("git")
        .args(["clone", "-q"])
        .arg(&remote)
        .arg(&clone)
        .status()
        .unwrap()
        .success());

    assert_eq!(
        git(&clone, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "quipu-ledger"
    );
    assert!(
        clone.join(a.uuid()).is_dir(),
        "the clone must actually hold the batches"
    );
}

#[test]
fn a_ledger_with_no_batches_in_it_names_the_branch_rather_than_denying_the_project() {
    let tmp = tempfile::tempdir().unwrap();
    let empty = tmp.path().join("nothing-published.git");
    assert!(std::process::Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&empty)
        .status()
        .unwrap()
        .success());
    let clone = tmp.path().join("by-hand");
    let _ = std::process::Command::new("git")
        .args(["clone", "-q"])
        .arg(&empty)
        .arg(&clone)
        .status();

    let db = tmp.path().join("b").join("db.sqlite");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let assert = qp(&db)
        .args(["init", "--from", clone.to_str().unwrap()])
        .assert()
        .failure();
    let err = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    // The message a person hits after cloning a ledger by hand must point at the
    // real cause. "No project here" alone sends them looking for a lost ledger.
    assert!(err.contains("quipu-ledger"), "must name the branch: {err}");
    assert!(err.contains("git clone"), "must name the cause: {err}");
}
