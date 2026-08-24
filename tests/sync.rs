//! `qp sync` — a real two-machine round trip over a local bare repository.
//!
//! Every "second machine" here is a SEPARATE `qp init`, never a copy of the
//! first. That is the whole point: a copied store shares the original's
//! `project_uuid` and mints the same display ids, so it cannot observe the
//! bugs that only appear between genuinely distinct peers — colliding `QP-1`s,
//! a batch filename that is not unique, an origin map that maps nothing.
//!
//! The remote is `git init --bare` in a temp dir. That is not a stand-in for a
//! hosted remote: it exercises identical ref and transport semantics, and it is
//! the topology the design settled on. No hostname, url or account belonging to
//! anyone appears in this file — the repo is public.

use assert_cmd::Command;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

fn qp(db: &Path) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env("QP_DB", db);
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

fn bare_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(format!("{name}.git"));
    let ok = std::process::Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&path)
        .status()
        .expect("spawn git")
        .success();
    assert!(ok, "git init --bare failed");
    path
}

/// A machine: its own store directory, its own `qp init`, its own uuid.
struct Machine {
    db: PathBuf,
}

impl Machine {
    /// A machine that STARTS a project: its own uuid and its own fresh key.
    fn new(root: &Path, name: &str, ledger: &Path) -> Self {
        Self::init(root, name, ledger, &["init"])
    }

    /// A machine that JOINS the project `key` — a second machine of one project.
    ///
    /// Still its own `qp init`, never a copy: a copied store would share the
    /// original's uuid, mint the same display ids and write to the same batch
    /// paths, so it could not observe anything a real second machine can. What
    /// it does not do is mint a second project, which is what plain `qp init`
    /// does and what makes two stores strangers that merely share a ledger.
    fn joining(root: &Path, name: &str, ledger: &Path, key: &str) -> Self {
        Self::init(root, name, ledger, &["init", "--project-key", key])
    }

    fn init(root: &Path, name: &str, ledger: &Path, args: &[&str]) -> Self {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("db.sqlite");
        ok(&db, args);
        ok(
            &db,
            &["remote", "set", "--ledger", ledger.to_str().unwrap()],
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

    fn sync(&self) -> Value {
        serde_json::from_str(&ok(&self.db, &["sync", "--json"])).unwrap()
    }

    fn ledger_dir(&self) -> PathBuf {
        self.db.parent().unwrap().join("ledger")
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

/// Every batch file in a ledger clone, path -> bytes.
fn ledger_files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name == ".git" || !e.path().is_dir() {
            continue;
        }
        for f in std::fs::read_dir(e.path()).unwrap().flatten() {
            let fname = f.file_name().to_string_lossy().to_string();
            if fname.ends_with(".jsonl") {
                out.insert(format!("{name}/{fname}"), std::fs::read(f.path()).unwrap());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// the round trip
// ---------------------------------------------------------------------------

#[test]
fn two_machines_exchange_their_tickets_through_the_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = bare_repo(tmp.path(), "ledger");
    let a = Machine::new(tmp.path(), "a", &remote);
    let b = Machine::joining(tmp.path(), "b", &remote, &a.key());

    // Genuinely distinct peers, which is what makes the rest of this test mean
    // anything. A copied store would fail here.
    assert_ne!(a.uuid(), b.uuid(), "two `qp init`s must mint two uuids");
    assert_eq!(a.key(), b.key(), "these two are meant to be one project");

    ok(&a.db, &["add", "written on machine a"]);
    ok(&a.db, &["add", "also on machine a"]);
    ok(&b.db, &["add", "written on machine b"]);

    // Both minted QP-1: the collision a same-uuid fake could never produce.
    assert_eq!(a.titles().len(), 2);
    assert_eq!(b.titles().len(), 1);

    a.sync(); // publishes a
    b.sync(); // fetches a, publishes b
    a.sync(); // fetches b

    let want = vec![
        "also on machine a".to_string(),
        "written on machine a".to_string(),
        "written on machine b".to_string(),
    ];
    assert_eq!(a.titles(), want, "machine a is missing b's ticket");
    assert_eq!(b.titles(), want, "machine b is missing a's tickets");
}

#[test]
fn a_second_sync_with_nothing_new_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = bare_repo(tmp.path(), "ledger");
    let a = Machine::new(tmp.path(), "a", &remote);
    let b = Machine::joining(tmp.path(), "b", &remote, &a.key());

    ok(&a.db, &["add", "from a"]);
    ok(&b.db, &["add", "from b"]);
    a.sync();
    b.sync();
    a.sync();

    let before = a.snapshot();
    let head_before = git(&a.ledger_dir(), &["rev-parse", "HEAD"]);
    let files_before = ledger_files(&a.ledger_dir());

    let out = a.sync();
    assert_eq!(out["pushed"], Value::from(false), "nothing to push");
    assert_eq!(out["fetched"], Value::Array(vec![]), "nothing to fetch");
    assert_eq!(out["import"], Value::Null, "no merge should have run");
    assert_eq!(out["export"]["event_count"], Value::from(0));
    assert_eq!(out["export"]["file"], Value::Null);

    assert_eq!(a.snapshot(), before, "an idle sync mutated the store");
    assert_eq!(
        git(&a.ledger_dir(), &["rev-parse", "HEAD"]),
        head_before,
        "an idle sync made a ledger commit"
    );
    assert_eq!(ledger_files(&a.ledger_dir()), files_before);
}

#[test]
fn sync_never_puts_a_claim_ref_on_the_ledger_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = bare_repo(tmp.path(), "ledger");
    let a = Machine::new(tmp.path(), "a", &remote);
    let b = Machine::joining(tmp.path(), "b", &remote, &a.key());

    ok(&a.db, &["add", "claimed work"]);
    ok(&a.db, &["assign", "QP-1", "--to", "agent-one"]);
    ok(&a.db, &["claim", "QP-1", "--as", "agent-one"]);
    ok(&b.db, &["add", "other work"]);
    a.sync();

    // A claim ref planted in the local ledger clone, standing in for a clone
    // that has one for any reason. It must not reach the remote: a second copy
    // of the authority's namespace is a second referee, and mutual exclusion
    // with two referees is not mutual exclusion. Widening either refspec —
    // `--mirror`, `--all`, `refs/*` — fails here.
    let head = git(&a.ledger_dir(), &["rev-parse", "HEAD"]);
    git(
        &a.ledger_dir(),
        &["update-ref", "refs/quipu/claims/QP-1", &head],
    );
    // New work on a, so the sync below genuinely pushes rather than finding
    // itself already up to date — a skipped push would make this vacuous.
    ok(&a.db, &["add", "more work"]);

    b.sync();
    assert_eq!(
        a.sync()["pushed"],
        Value::from(true),
        "the sync under test must actually push, or this proves nothing"
    );

    assert_eq!(
        git(
            &a.ledger_dir(),
            &["for-each-ref", "--format=%(refname)", "refs/quipu/"]
        ),
        "refs/quipu/claims/QP-1",
        "the planted local claim ref vanished, so this test proves nothing"
    );
    let claims = git(
        &remote,
        &["for-each-ref", "--format=%(refname)", "refs/quipu/"],
    );
    assert!(
        claims.is_empty(),
        "sync wrote into the authority's namespace: {claims}"
    );
    let refs = git(&remote, &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(
        refs, "refs/heads/quipu-ledger",
        "sync published something other than the one ledger branch"
    );
}

#[test]
fn sync_never_rewrites_a_batch_another_store_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = bare_repo(tmp.path(), "ledger");
    let a = Machine::new(tmp.path(), "a", &remote);
    let b = Machine::joining(tmp.path(), "b", &remote, &a.key());

    ok(&a.db, &["add", "a first"]);
    ok(&b.db, &["add", "b first"]);
    a.sync();
    b.sync();
    a.sync();

    let peers_before: BTreeMap<String, Vec<u8>> = ledger_files(&a.ledger_dir())
        .into_iter()
        .filter(|(k, _)| k.starts_with(&b.uuid()))
        .collect();
    assert!(!peers_before.is_empty(), "a never received b's batches");

    // More rounds, so there is something for both sides to write.
    ok(&b.db, &["add", "b second"]);
    ok(&a.db, &["add", "a second"]);
    b.sync();
    a.sync();

    let after = ledger_files(&a.ledger_dir());
    for (path, bytes) in &peers_before {
        assert_eq!(
            after.get(path),
            Some(bytes),
            "{path} was rewritten or removed by a later sync"
        );
    }
}

#[test]
fn a_store_with_no_ledger_remote_fails_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("solo");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("db.sqlite");
    ok(&db, &["init"]);
    ok(&db, &["add", "local only"]);

    let conn = rusqlite::Connection::open(&db).unwrap();
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))
        .unwrap();

    let out = qp(&db).arg("sync").assert().failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(
        stderr.contains("no ledger remote") && stderr.contains("qp remote set --ledger"),
        "unhelpful failure: {stderr}"
    );

    assert!(
        !dir.join("ledger").exists(),
        "a failed sync created a ledger clone anyway"
    );
    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after, "a failed sync wrote to the store");
}

#[test]
fn an_authority_url_is_never_used_as_a_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let dir = tmp.path().join("solo");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("db.sqlite");
    ok(&db, &["init"]);
    ok(
        &db,
        &["remote", "set", "--authority", authority.to_str().unwrap()],
    );
    ok(&db, &["add", "local only"]);

    qp(&db).arg("sync").assert().failure();

    let refs = git(&authority, &["for-each-ref", "--format=%(refname)"]);
    assert!(
        refs.is_empty(),
        "sync fell back to the authority url: {refs}"
    );
}

#[test]
fn a_sync_leaves_nothing_for_the_project_repo_to_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = bare_repo(tmp.path(), "ledger");

    // A project repo shaped like a real one: a store under `.quipu/`, which the
    // project already ignores. The owner's requirement is that a sync must never
    // leave a file a person could accidentally commit.
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
    ok(&db, &["init"]);
    ok(
        &db,
        &["remote", "set", "--ledger", remote.to_str().unwrap()],
    );
    ok(&db, &["add", "work"]);

    let peer = Machine::new(tmp.path(), "peer", &remote);
    ok(&peer.db, &["add", "peer work"]);
    peer.sync();

    ok(&db, &["sync"]);

    assert!(
        db.parent().unwrap().join("ledger").exists(),
        "the ledger clone should live under the store directory"
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
    let remote = bare_repo(tmp.path(), "ledger");
    let a1 = Machine::new(tmp.path(), "a1", &remote);
    let a2 = Machine::joining(tmp.path(), "a2", &remote, &a1.key());
    let b = Machine::new(tmp.path(), "b", &remote);

    assert_eq!(a1.key(), a2.key(), "a1 and a2 are one project");
    assert_ne!(a1.key(), b.key(), "b is a different project");
    assert_ne!(a1.uuid(), a2.uuid(), "peers are separate stores");

    ok(&a1.db, &["add", "first machine of the first project"]);
    ok(&a2.db, &["add", "second machine of the first project"]);
    ok(&b.db, &["add", "the other project entirely"]);

    // Two rounds each, so everyone has both published and fetched.
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

    // The other project's batches ARE in everyone's clone. Scoping the import is
    // the fix; scoping the fetch is not, and a ledger anyone can clone whole is
    // what a global read across projects depends on.
    let files = ledger_files(&a1.ledger_dir());
    assert!(
        files.keys().any(|k| k.starts_with(&b.uuid())),
        "the stranger's batches were never fetched, so this proves the wrong thing"
    );

    // The merge itself, aimed straight at the whole ledger the way sync aims
    // it. Asserted separately because sync also declines to call it when
    // nothing of ours arrived: with only that check in place, an unscoped merge
    // would still absorb the ledger the moment anything else invoked one.
    let out = ok(
        &a1.db,
        &[
            "import",
            "--from",
            a1.ledger_dir().to_str().unwrap(),
            "--merge",
            "--json",
        ],
    );
    let out: Value = serde_json::from_str(&out).unwrap();
    assert!(
        out["foreign_batches"].as_u64().unwrap() > 0,
        "the merge saw no other project at all, so this proves the wrong thing"
    );
    assert_eq!(
        a1.titles(),
        project_a,
        "a merge pointed at the ledger root absorbed another project"
    );
}

/// The stranger's arrival must not read as "something new for me".
#[test]
fn another_projects_batches_are_not_something_this_store_has_to_merge() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = bare_repo(tmp.path(), "ledger");
    let a = Machine::new(tmp.path(), "a", &remote);
    let b = Machine::new(tmp.path(), "b", &remote);

    ok(&a.db, &["add", "mine"]);
    a.sync();
    ok(&b.db, &["add", "theirs"]);
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
        "a stranger's push disturbed this store"
    );

    // And it really did arrive in the clone — otherwise the assertions above
    // would hold for a store that simply fetched nothing.
    assert!(
        ledger_files(&a.ledger_dir())
            .keys()
            .any(|k| k.starts_with(&b.uuid())),
        "b's batch never reached a's clone"
    );
}
