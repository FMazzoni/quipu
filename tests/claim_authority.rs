//! `qp claim` against a claim authority: the ref is acquired before anything local moves.
//!
//! Every authority here is a bare repository in the test's own temp dir.
//! `git init --bare` exercises identical ref and transport semantics to a
//! hosted remote, and it belongs to nobody — no host, no account, no url that
//! could leak out of a public repo. Nothing in this file reaches a network.
//!
//! Two of these tests are about the *primitive* rather than about quipu, and
//! they earn their place: the naive push and the shared claim object are both
//! silently broken in ways that a plausible-looking test does not catch, and a
//! future refactor that reaches for either would otherwise go green.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// the happy path
// ---------------------------------------------------------------------------

/// The first claimant wins, and what lands on the authority is a blob under an
/// opaque name.
#[test]
fn the_first_claimant_acquires_an_opaque_blob_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let db = store(tmp.path(), "a", &authority);
    assign_one(&db, "agent-a");

    qp(&db)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();

    let refs = claim_refs(&authority);
    assert_eq!(refs.len(), 1, "exactly one claim ref, got {refs:?}");
    let (name, sha) = &refs[0];
    assert!(name.starts_with("refs/quipu/claims/"));
    assert!(
        !name.contains("QP"),
        "the ref name must not carry the display id: {name}"
    );
    assert!(name
        .rsplit('/')
        .next()
        .is_some_and(|h| h.len() == 40 && h.chars().all(|c| c.is_ascii_hexdigit())));
    assert_eq!(
        object_type(&authority, sha),
        "blob",
        "a commit would publish the operator's name and email and drag a parent chain"
    );
    let body = object_body(&authority, sha);
    assert!(body.contains("agent-a"), "body was {body}");
    assert!(
        !body.contains("QP-1"),
        "the ticket id must not cross: {body}"
    );
}

/// No authority configured: unchanged behaviour, and not one subprocess.
#[test]
fn with_no_authority_the_claim_is_local_and_spawns_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("db.sqlite");
    qp(&db).arg("init").assert().success();
    assign_one(&db, "agent-a");

    qp(&db)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();
    qp(&db)
        .args(["complete", "QP-1", "--as", "agent-a"])
        .assert()
        .success();

    assert_eq!(state(&db, "QP-1"), "done");
    assert!(
        !dir.join("claims.git").exists(),
        "a local-only store must never create a claim repository"
    );
    assert!(!dir.join("ssh").exists());
}

/// The claim path must not depend on ambient git identity.
///
/// Stripping `HOME` removes config but not the passwd-derived fallback ident,
/// so the name and email are blanked explicitly here. A blob has no author and
/// no committer, which is why this passes — and why it would stop passing the
/// moment someone reached for `commit-tree`.
#[test]
fn claiming_needs_no_git_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let db = store(tmp.path(), "a", &authority);
    assign_one(&db, "agent-a");

    let mut c = qp(&db);
    c.env_remove("HOME")
        .env("GIT_AUTHOR_NAME", "")
        .env("GIT_AUTHOR_EMAIL", "")
        .env("GIT_COMMITTER_NAME", "")
        .env("GIT_COMMITTER_EMAIL", "")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
    c.args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();

    assert_eq!(claim_refs(&authority).len(), 1);
}

// ---------------------------------------------------------------------------
// the refusal
// ---------------------------------------------------------------------------

/// A second store sharing the authority is refused, and its own state does not move.
#[test]
fn a_second_store_is_refused_and_mutates_nothing_locally() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let a = store(tmp.path(), "a", &authority);
    assign_one(&a, "agent-a");
    let b = replica(&a, tmp.path(), "b", &authority, "agent-b");

    qp(&a)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();

    let err = json_err(&b, &["claim", "QP-1", "--as", "agent-b", "--json"]);
    assert_eq!(err["code"], Value::from("claim_held_remotely"));
    assert_eq!(
        state(&b, "QP-1"),
        "assigned",
        "a refused claim must leave the loser's store untouched"
    );
    assert_eq!(claim_refs(&authority).len(), 1);
}

/// Unreachable authority: the claim fails closed and nothing local moves.
///
/// There is deliberately no local-only fallback. A fallback would hand out the
/// duplicate claim the authority exists to refuse.
#[test]
fn an_unreachable_authority_fails_the_claim_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nowhere.git");
    let db = store(tmp.path(), "a", missing.to_str().unwrap());
    assign_one(&db, "agent-a");

    let err = json_err(&db, &["claim", "QP-1", "--as", "agent-a", "--json"]);
    assert_eq!(err["code"], Value::from("authority_unreachable"));
    assert!(
        err["message"].as_str().unwrap().contains("qp remote clear"),
        "the unreachable message must name the way out: {}",
        err["message"]
    );
    assert_eq!(state(&db, "QP-1"), "assigned");

    // And the ticket is still claimable once the authority comes back.
    let authority = bare_repo(tmp.path(), "authority");
    qp(&db)
        .args(["remote", "set", "--authority", &authority])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();
    assert_eq!(state(&db, "QP-1"), "running");
}

/// A claim the local guard refuses must give the ref back.
///
/// Otherwise the acquisition outlives the attempt it belonged to and the ticket
/// stays locked on the authority by a store that never started the work.
#[test]
fn a_locally_rejected_claim_releases_the_ref_it_acquired() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let db = store(tmp.path(), "a", &authority);
    assign_one(&db, "agent-a");

    qp(&db)
        .args(["claim", "QP-1", "--as", "someone-else"])
        .assert()
        .failure();

    assert!(
        claim_refs(&authority).is_empty(),
        "the ref must not survive a claim the local guard rejected"
    );
    qp(&db)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// release
// ---------------------------------------------------------------------------

#[test]
fn completing_deletes_the_ref_and_the_ticket_becomes_claimable_again() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let a = store(tmp.path(), "a", &authority);
    assign_one(&a, "agent-a");
    let b = replica(&a, tmp.path(), "b", &authority, "agent-b");

    qp(&a)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();
    assert_eq!(claim_refs(&authority).len(), 1);

    qp(&a)
        .args(["complete", "QP-1", "--as", "agent-a"])
        .assert()
        .success();
    assert!(
        claim_refs(&authority).is_empty(),
        "release must delete the ref"
    );

    qp(&b)
        .args(["claim", "QP-1", "--as", "agent-b"])
        .assert()
        .success();
    assert_eq!(claim_refs(&authority).len(), 1);
}

#[test]
fn abandon_and_cancel_release_the_ref_too() {
    for release in ["abandon", "cancel"] {
        let tmp = tempfile::tempdir().unwrap();
        let authority = bare_repo(tmp.path(), "authority");
        let db = store(tmp.path(), "a", &authority);
        assign_one(&db, "agent-a");
        qp(&db)
            .args(["claim", "QP-1", "--as", "agent-a"])
            .assert()
            .success();

        let mut c = qp(&db);
        c.args([release, "QP-1"]);
        if release == "abandon" {
            c.args(["--as", "agent-a"]);
        }
        c.assert().success();

        assert!(
            claim_refs(&authority).is_empty(),
            "`{release}` must delete the claim ref"
        );
    }
}

#[test]
fn reclaim_releases_the_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let db = store(tmp.path(), "a", &authority);
    assign_one(&db, "agent-a");
    qp(&db)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();

    qp(&db).args(["reclaim", "QP-1"]).assert().success();
    assert!(claim_refs(&authority).is_empty());
}

/// Two attempts at the same ticket must mint different objects.
///
/// If they did not, git would answer the second one `Everything up-to-date`
/// with exit 0 and tell it that it won. Claim, release, claim again: the ref
/// must land on a different sha.
#[test]
fn every_attempt_pushes_a_distinct_object() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let db = store(tmp.path(), "a", &authority);
    assign_one(&db, "agent-a");

    qp(&db)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();
    let first = claim_refs(&authority)[0].1.clone();
    qp(&db)
        .args(["abandon", "QP-1", "--as", "agent-a"])
        .assert()
        .success();
    qp(&db)
        .args(["assign", "QP-1", "--to", "agent-a"])
        .assert()
        .success();
    qp(&db)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();
    let second = claim_refs(&authority)[0].1.clone();

    assert_ne!(
        first, second,
        "two claimants pushing one shared object are both told they won"
    );
}

// ---------------------------------------------------------------------------
// the race
// ---------------------------------------------------------------------------

/// Two stores claiming at once: exactly one wins, and no winner is a no-op.
///
/// Asserting the exit code alone is not enough. A `Everything up-to-date` push
/// exits 0 while having arbitrated nothing, so the output is checked too.
#[test]
fn two_concurrent_claimants_produce_exactly_one_winner() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let a = store(tmp.path(), "a", &authority);
    assign_one(&a, "agent-a");
    let b = replica(&a, tmp.path(), "b", &authority, "agent-b");

    let one = |db: PathBuf, agent: &'static str| {
        std::thread::spawn(move || {
            qp(&db)
                .args(["claim", "QP-1", "--as", agent])
                .output()
                .unwrap()
        })
    };
    let ha = one(a.clone(), "agent-a");
    let hb = one(b.clone(), "agent-b");
    let outs = [ha.join().unwrap(), hb.join().unwrap()];

    let winners = outs.iter().filter(|o| o.status.success()).count();
    assert_eq!(winners, 1, "exactly one claimant may win");
    for o in &outs {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(
            !text.contains("up-to-date"),
            "a no-op push is a false winner: {text}"
        );
    }
    assert_eq!(claim_refs(&authority).len(), 1);
    let running = [&a, &b]
        .iter()
        .filter(|d| state(d, "QP-1") == "running")
        .count();
    assert_eq!(running, 1, "exactly one store may record the claim");
}

// ---------------------------------------------------------------------------
// peers, which is the topology the product actually ships
// ---------------------------------------------------------------------------

/// Two genuinely independent stores, one shared authority, one logical ticket.
///
/// This is the case the replica tests above cannot reach. Peers are not
/// replicas: each keeps its own `project_uuid`, and the same logical ticket is
/// numbered differently in each — `QP-1` here is `QP-3` there, and both are
/// correct local names. A ref keyed on the *asking* store's uuid or on a
/// display id therefore makes two peers compute two different names, contend
/// over nothing, and both exit 0 holding the same ticket. Measured before the
/// fix: two refs on the authority, both stores `running`.
///
/// Neither store is copied from the other. They are separate `qp init` calls
/// joined by the real export/import path, which is the only way this assertion
/// means anything.
#[test]
fn two_peers_numbering_a_ticket_differently_still_contend() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let (a, b) = peers(tmp.path(), &authority);

    assert_eq!(title(&a, "QP-1"), "shared work item");
    assert_eq!(
        title(&b, "QP-3"),
        "shared work item",
        "the peer must number the same ticket differently, or this proves nothing"
    );

    qp(&a)
        .args(["assign", "QP-1", "--to", "agent-a"])
        .assert()
        .success();
    qp(&b)
        .args(["assign", "QP-3", "--to", "agent-b"])
        .assert()
        .success();

    let first = qp(&a)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .output()
        .unwrap();
    let second = qp(&b)
        .args(["claim", "QP-3", "--as", "agent-b"])
        .output()
        .unwrap();

    let winners = [&first, &second]
        .iter()
        .filter(|o| o.status.success())
        .count();
    assert_eq!(winners, 1, "exactly one peer may hold one logical ticket");
    assert_eq!(
        claim_refs(&authority).len(),
        1,
        "two refs means the peers never contended"
    );
    assert_eq!(state(&a, "QP-1"), "running");
    assert_eq!(
        state(&b, "QP-3"),
        "assigned",
        "the loser's store must be untouched"
    );
}

/// The same peers, racing. Exactly one winner, and no winner is a no-op.
#[test]
fn two_peers_claiming_concurrently_produce_exactly_one_winner() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let (a, b) = peers(tmp.path(), &authority);
    qp(&a)
        .args(["assign", "QP-1", "--to", "agent-a"])
        .assert()
        .success();
    qp(&b)
        .args(["assign", "QP-3", "--to", "agent-b"])
        .assert()
        .success();

    let one = |db: PathBuf, task: &'static str, agent: &'static str| {
        std::thread::spawn(move || {
            qp(&db)
                .args(["claim", task, "--as", agent])
                .output()
                .unwrap()
        })
    };
    let ha = one(a.clone(), "QP-1", "agent-a");
    let hb = one(b.clone(), "QP-3", "agent-b");
    let outs = [ha.join().unwrap(), hb.join().unwrap()];

    assert_eq!(
        outs.iter().filter(|o| o.status.success()).count(),
        1,
        "exactly one peer may win"
    );
    for o in &outs {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(
            !text.contains("up-to-date"),
            "a no-op push is a false winner: {text}"
        );
    }
    assert_eq!(claim_refs(&authority).len(), 1);
    let running = [(&a, "QP-1"), (&b, "QP-3")]
        .iter()
        .filter(|(d, t)| state(d, t) == "running")
        .count();
    assert_eq!(running, 1, "exactly one store may record the claim");
}

/// The peer that lost may claim once the holder releases.
#[test]
fn a_peer_can_claim_after_the_holder_releases() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let (a, b) = peers(tmp.path(), &authority);
    qp(&a)
        .args(["assign", "QP-1", "--to", "agent-a"])
        .assert()
        .success();
    qp(&b)
        .args(["assign", "QP-3", "--to", "agent-b"])
        .assert()
        .success();

    qp(&a)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();
    qp(&b)
        .args(["claim", "QP-3", "--as", "agent-b"])
        .assert()
        .failure();
    qp(&a)
        .args(["complete", "QP-1", "--as", "agent-a"])
        .assert()
        .success();
    assert!(claim_refs(&authority).is_empty());

    qp(&b)
        .args(["claim", "QP-3", "--as", "agent-b"])
        .assert()
        .success();
    assert_eq!(claim_refs(&authority).len(), 1);
}

/// A ticket each peer minted for itself is two tickets, and both may be held.
///
/// The mirror of the test above, and it is what stops the fix from being "hash
/// one constant": distinct canonical identities must still get distinct refs.
#[test]
fn two_unrelated_tickets_do_not_contend() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let (a, b) = peers(tmp.path(), &authority);

    // QP-1 in store B is B's own "local noise", minted there — nothing to do
    // with A's QP-1 beyond sharing a local number.
    qp(&a)
        .args(["assign", "QP-1", "--to", "agent-a"])
        .assert()
        .success();
    qp(&b)
        .args(["assign", "QP-1", "--to", "agent-b"])
        .assert()
        .success();
    qp(&a)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();
    qp(&b)
        .args(["claim", "QP-1", "--as", "agent-b"])
        .assert()
        .success();

    assert_eq!(
        claim_refs(&authority).len(),
        2,
        "two different tickets that happen to share a local number must not collide"
    );
}

/// `block` is a release edge too, and a stranded ref is an unclaimable ticket.
#[test]
fn blocking_releases_the_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let db = store(tmp.path(), "a", &authority);
    assign_one(&db, "agent-a");
    qp(&db)
        .args(["claim", "QP-1", "--as", "agent-a"])
        .assert()
        .success();

    qp(&db)
        .args(["block", "QP-1", "--as", "agent-a", "--new", "a blocker"])
        .assert()
        .success();

    assert_eq!(state(&db, "QP-1"), "pending");
    assert!(
        claim_refs(&authority).is_empty(),
        "`block` releases the claim, so it must release the ref"
    );
}

// ---------------------------------------------------------------------------
// the primitive, pinned
// ---------------------------------------------------------------------------

/// The naive push is broken, and the lease is what fixes it.
///
/// Refs outside `refs/heads` and `refs/tags` accept any fast-forward, so a
/// second claimant whose object *descends* from the winner's is accepted with
/// exit 0 by a plain push and silently steals the claim. Racing two unrelated
/// objects passes against the broken primitive, which is why the descendant
/// case is the one pinned here. Both halves are asserted: without the lease it
/// is accepted, with the lease it is refused.
#[test]
fn a_descendant_steals_the_ref_without_the_lease_and_is_refused_with_it() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let work = tmp.path().join("work");
    let name = "refs/quipu/claims/deadbeef";

    // A chain: c2 descends from c1. Identity is supplied here because this test
    // is about commits by construction — quipu itself never makes one.
    init_work(&work);
    let c1 = commit(&work, "one");
    let c2 = commit(&work, "two");
    assert_eq!(
        git(&work, &["merge-base", "--is-ancestor", &c1, &c2]).0,
        0,
        "the trap only exists when the second object descends from the first"
    );

    let (rc, _) = git(&work, &["push", &authority, &format!("{c1}:{name}")]);
    assert_eq!(rc, 0, "the first claimant lands the ref");

    // The refutation: the naive push takes it.
    let (rc, _) = git(&work, &["push", &authority, &format!("{c2}:{name}")]);
    assert_eq!(
        rc, 0,
        "a plain push of a descendant is ACCEPTED — this is why the lease is mandatory"
    );

    // And with the lease, the same attempt is refused.
    let c3 = commit(&work, "three");
    let (rc, out) = git(
        &work,
        &[
            "push",
            &format!("--force-with-lease={name}:"),
            &authority,
            &format!("{c3}:{name}"),
        ],
    );
    assert_eq!(rc, 1, "an empty lease means the ref must not already exist");
    assert!(!out.contains("up-to-date"), "{out}");
}

/// Pushing the object a ref already holds exits 0 while arbitrating nothing.
///
/// This is the shape that told five of ten concurrent agents they had won. It
/// is pinned here so a refactor that anchors every claim at a shared object
/// fails a test instead of shipping.
#[test]
fn re_pushing_the_same_object_is_a_no_op_that_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = bare_repo(tmp.path(), "authority");
    let work = tmp.path().join("work");
    let name = "refs/quipu/claims/cafebabe";
    init_work(&work);
    let c1 = commit(&work, "one");

    let (rc, _) = git(&work, &["push", &authority, &format!("{c1}:{name}")]);
    assert_eq!(rc, 0);
    let (rc, out) = git(
        &work,
        &[
            "push",
            &format!("--force-with-lease={name}:"),
            &authority,
            &format!("{c1}:{name}"),
        ],
    );
    assert_eq!(rc, 0, "git exits 0 for a push it had nothing to do for");
    assert!(
        out.contains("Everything up-to-date"),
        "the only signal that this win is fake: {out}"
    );
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn bare_repo(root: &Path, name: &str) -> String {
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

/// A fresh store in its own directory, attached to `authority`.
fn store(root: &Path, name: &str, authority: &str) -> PathBuf {
    store_of(root, name, authority, None)
}

/// The project the peers below are two machines of.
///
/// A merge is scoped to a project, so two plain `qp init`s would be two
/// projects sharing a directory and `peers` would hand back stores that never
/// exchanged the ticket they are supposed to contend over. Naming the project
/// is what keeps them peers; they are still separate `qp init` calls, and their
/// `project_uuid`s still differ, which is what the assertion in `peers` checks.
const PROJECT: &str = "one-project-under-test";

fn store_of(root: &Path, name: &str, authority: &str, key: Option<&str>) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("db.sqlite");
    let mut init = qp(&db);
    init.arg("init");
    if let Some(key) = key {
        init.args(["--project-key", key]);
    }
    init.assert().success();
    qp(&db)
        .args(["remote", "set", "--authority", authority])
        .assert()
        .success();
    db
}

/// A byte copy of `src`, sharing its `project_uuid`.
///
/// This is the *disaster-recovery* topology, not the sync one, and the tests
/// using it are about the mechanism rather than about identity. `peers` builds
/// the topology the product ships; see the note there for why a copy cannot
/// stand in for it.
fn replica(src: &Path, root: &Path, name: &str, authority: &str, agent: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("db.sqlite");
    for suffix in ["", "-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{suffix}", src.display()));
        if from.exists() {
            std::fs::copy(&from, format!("{}{suffix}", db.display())).unwrap();
        }
    }
    qp(&db)
        .args(["remote", "set", "--authority", authority])
        .assert()
        .success();
    qp(&db).args(["reclaim", "QP-1"]).assert().success();
    qp(&db)
        .args(["assign", "QP-1", "--to", agent])
        .assert()
        .success();
    db
}

/// Two independent stores sharing one logical ticket, numbered differently.
///
/// Separate `qp init` calls, so the `project_uuid`s genuinely differ, joined by
/// the real `export` -> `import --merge` path. `B` is seeded with two tasks of
/// its own first, so the shared ticket lands on `QP-3` there against `QP-1` in
/// `A` — which is the normal case, and the one a display-id-keyed ref breaks on.
fn peers(root: &Path, authority: &str) -> (PathBuf, PathBuf) {
    let a = store_of(root, "a", authority, Some(PROJECT));
    let b = store_of(root, "b", authority, Some(PROJECT));
    qp(&a).args(["add", "shared work item"]).assert().success();
    qp(&b).args(["add", "local noise"]).assert().success();
    qp(&b).args(["add", "more noise"]).assert().success();

    let ledger = root.join("ledger");
    qp(&a)
        .args(["export", "--out"])
        .arg(&ledger)
        .assert()
        .success();
    qp(&b)
        .args(["import", "--merge", "--from"])
        .arg(&ledger)
        .assert()
        .success();

    // Both export into one root: two directories means two identities. A copied
    // store would produce one, and every assertion downstream would be vacuous.
    qp(&b)
        .args(["export", "--out"])
        .arg(&ledger)
        .assert()
        .success();
    let stores = std::fs::read_dir(&ledger)
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().is_dir())
        .count();
    assert_eq!(stores, 2, "the peers must not share a project_uuid");
    (a, b)
}

fn title(db: &Path, task: &str) -> String {
    let out = qp(db).args(["show", task, "--json"]).output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["title"].as_str().unwrap().to_string()
}

fn assign_one(db: &Path, agent: &str) {
    qp(db).args(["add", "a task"]).assert().success();
    qp(db)
        .args(["assign", "QP-1", "--to", agent])
        .assert()
        .success();
}

fn state(db: &Path, task: &str) -> String {
    let out = qp(db).args(["show", task, "--json"]).output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["state"].as_str().unwrap().to_string()
}

/// Runs a command expected to fail and returns the `--json` error body.
///
/// The envelope is the *last* line of stderr: quipu may print a
/// `{"warning": ...}` line ahead of it when the cwd holds a different store
/// than `QP_DB` names, which is exactly the situation a test in a temp dir is
/// in when the checkout it runs from has a store of its own.
fn json_err(db: &Path, args: &[&str]) -> Value {
    let out = qp(db).args(args).output().unwrap();
    assert!(!out.status.success(), "expected a failure from {args:?}");
    let text = String::from_utf8_lossy(&out.stderr);
    let last = text.lines().rfind(|l| !l.trim().is_empty());
    let v: Value = serde_json::from_str(last.unwrap_or_default())
        .unwrap_or_else(|e| panic!("stderr was not json ({e}): {text}"));
    v["error"].clone()
}

/// Every `refs/quipu/claims/*` on the authority, as (name, sha).
fn claim_refs(authority: &str) -> Vec<(String, String)> {
    let out = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(authority)
        .args([
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/quipu/claims",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn object_type(authority: &str, sha: &str) -> String {
    cat_file(authority, "-t", sha)
}

fn object_body(authority: &str, sha: &str) -> String {
    cat_file(authority, "-p", sha)
}

fn cat_file(authority: &str, flag: &str, sha: &str) -> String {
    let out = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(authority)
        .args(["cat-file", flag, sha])
        .output()
        .unwrap();
    assert!(out.status.success(), "git cat-file {flag} {sha} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A throwaway working repo for the two primitive tests. Identity is pinned
/// here rather than inherited, so the tests do not depend on ambient config.
fn init_work(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    assert_eq!(git(dir, &["init", "-q"]).0, 0);
    assert_eq!(git(dir, &["config", "user.name", "t"]).0, 0);
    assert_eq!(
        git(dir, &["config", "user.email", "t@example.invalid"]).0,
        0
    );
}

fn commit(dir: &Path, msg: &str) -> String {
    assert_eq!(
        git(dir, &["commit", "-q", "--allow-empty", "-m", msg]).0,
        0,
        "commit failed"
    );
    git(dir, &["rev-parse", "HEAD"]).1.trim().to_string()
}

fn git(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// `qp` with the ambient store scrubbed, so an exported `QP_DB` cannot redirect
/// a test at a real store.
fn qp(db: &Path) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env_remove("QP_DB");
    c.env("QP_DB", db);
    c
}
