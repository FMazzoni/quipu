//! The object-store transport itself: what goes on the wire, and what comes back.
//!
//! `sync.rs` proves the exchange converges. This file proves the requests that
//! carry it are the right requests — the method, the path, the signed query, the
//! pagination loop — and that a failure keeps the cause a person could act on
//! instead of collapsing to "request failed". A follow-up `qp remote doctor` can
//! only distinguish causes the client preserved, so they are pinned here.
//!
//! Hermetic throughout: an in-process stub on a loopback port, obviously fake
//! credentials, no hostname or bucket belonging to anyone.

mod common;

use assert_cmd::Command;
use common::objstore::{ObjectStore, BUCKET};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn qp(db: &Path, store: &ObjectStore) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    c.env("QP_DB", db);
    for (k, v) in store.env() {
        c.env(k, v);
    }
    c
}

/// A store attached to `url`, with one ticket in it and nothing synced yet.
fn seeded(root: &Path, store: &ObjectStore, url: &str) -> PathBuf {
    let dir = root.join("s");
    let _ = std::fs::create_dir_all(root);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("db.sqlite");
    qp(&db, store).arg("init").assert().success();
    qp(&db, store)
        .args(["remote", "set", "--ledger", url])
        .assert()
        .success();
    qp(&db, store).args(["add", "work"]).assert().success();
    db
}

// ---------------------------------------------------------------------------
// the wire
// ---------------------------------------------------------------------------

/// One sync, read off the socket: the requests, their shape, and their signing.
#[test]
fn a_sync_issues_a_signed_list_and_a_signed_put() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let db = seeded(tmp.path(), &store, &store.url());
    qp(&db, &store).arg("sync").assert().success();

    let reqs = store.requests();
    assert!(!reqs.is_empty(), "the sync issued no requests");

    let list = reqs
        .iter()
        .find(|r| r.query().contains("list-type=2"))
        .expect("no ListObjectsV2 request was made");
    assert_eq!(list.method, "GET", "a listing must be a GET");
    assert_eq!(
        list.path(),
        format!("/{BUCKET}/"),
        "path-style addressing puts the bucket in the path"
    );
    assert!(
        list.header("host").is_some(),
        "no Host header; SigV4 signs it and every S3 server requires it"
    );

    let put = reqs
        .iter()
        .find(|r| r.method == "PUT")
        .expect("the sync published nothing");
    assert!(
        put.path().starts_with(&format!("/{BUCKET}/")),
        "a PUT went somewhere other than the bucket: {}",
        put.path()
    );
    assert_eq!(
        put.header("content-length").map(str::to_string),
        Some(
            store
                .keys()
                .first()
                .and_then(|k| store.get(k))
                .map(|b| b.len().to_string())
                .unwrap()
        ),
        "the PUT body length does not match the object stored"
    );

    // Presigned urls, so the credential scope and the signature ride in the
    // query rather than in an Authorization header — there is deliberately no
    // header to inspect. What must be true of every signed request:
    for r in [list, put] {
        let q = r.query();
        for part in [
            "X-Amz-Algorithm=AWS4-HMAC-SHA256",
            "X-Amz-Credential=",
            "X-Amz-Date=",
            "X-Amz-Expires=",
            "X-Amz-SignedHeaders=host",
            "X-Amz-Signature=",
        ] {
            assert!(q.contains(part), "unsigned or malformed request: {q}");
        }
        assert!(
            !q.contains("wJalrXUtnFEMI"),
            "the secret key leaked into the request: {q}"
        );
        assert!(
            q.contains("%2Fus-east-1%2Fs3%2Faws4_request"),
            "the credential scope is not an S3 scope: {q}"
        );
    }
}

/// A ledger url may carry a key prefix, and everything must stay under it.
#[test]
fn a_url_prefix_scopes_every_key_the_store_touches() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let db = seeded(tmp.path(), &store, &store.url_with_prefix("ledgers/one"));
    qp(&db, &store).arg("sync").assert().success();

    let keys = store.keys();
    assert!(!keys.is_empty(), "nothing was published");
    for k in &keys {
        assert!(
            k.starts_with("ledgers/one/"),
            "{k} escaped the url's key prefix"
        );
    }
    // And a second sync finds its own work again through the same prefix,
    // rather than republishing because it could not see it.
    let out = qp(&db, &store)
        .args(["sync", "--json"])
        .output()
        .expect("run sync");
    let out: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        out["pushed"],
        Value::from(false),
        "a prefixed store could not find the batch it had just written"
    );
}

/// A listing longer than one page must be followed to the end.
#[test]
fn a_listing_longer_than_one_page_is_followed_to_the_end() {
    let tmp = tempfile::tempdir().unwrap();
    // One key per page, so any run with more than one object paginates.
    let store = ObjectStore::with_page_size(1);
    assert_eq!(store.page_size(), 1);

    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let (adb, bdb) = (a.join("db.sqlite"), b.join("db.sqlite"));

    qp(&adb, &store).arg("init").assert().success();
    qp(&adb, &store)
        .args(["remote", "set", "--ledger", &store.url()])
        .assert()
        .success();
    let key: String = {
        let conn = rusqlite::Connection::open(&adb).unwrap();
        conn.query_row(
            "SELECT value FROM meta WHERE key = 'project_key'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    qp(&bdb, &store)
        .args(["init", "--project-key", &key])
        .assert()
        .success();
    qp(&bdb, &store)
        .args(["remote", "set", "--ledger", &store.url()])
        .assert()
        .success();

    // Three batches from two stores, so the listing spans three pages.
    qp(&adb, &store).args(["add", "one"]).assert().success();
    qp(&adb, &store).arg("sync").assert().success();
    qp(&adb, &store).args(["add", "two"]).assert().success();
    qp(&adb, &store).arg("sync").assert().success();
    qp(&bdb, &store).args(["add", "three"]).assert().success();
    qp(&bdb, &store).arg("sync").assert().success();
    assert_eq!(store.keys().len(), 3);

    store.forget_requests();
    qp(&adb, &store).arg("sync").assert().success();
    let pages = store
        .requests()
        .iter()
        .filter(|r| r.query().contains("list-type=2"))
        .count();
    assert!(
        pages > 1,
        "the client stopped after one page; a truncated listing would silently \
         hide half the ledger"
    );

    // And it really did see everything: b's ticket arrived.
    let conn = rusqlite::Connection::open(&adb).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM task", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3, "pagination lost a batch");
}

// ---------------------------------------------------------------------------
// failures stay distinguishable
// ---------------------------------------------------------------------------

/// Absent credentials are not "unreachable", and the message names the variables.
#[test]
fn missing_credentials_say_which_variables_were_looked_for() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let db = seeded(tmp.path(), &store, &store.url());

    let out = qp(&db, &store)
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .arg("sync")
        .assert()
        .failure();
    let err = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(
        err.contains("AWS_ACCESS_KEY_ID") && err.contains("AWS_SECRET_ACCESS_KEY"),
        "the refusal does not name the variables it looked for: {err}"
    );
    assert!(
        !err.contains("unreachable"),
        "absent credentials were reported as an unreachable endpoint: {err}"
    );
    assert!(store.keys().is_empty(), "a credential-less sync published");
}

/// Every S3 error code the doctor will need keeps its own diagnosis.
#[test]
fn each_failure_cause_survives_as_itself() {
    // (status, S3 code, a phrase only that diagnosis would produce)
    let cases = [
        (403, "SignatureDoesNotMatch", "rejected the credentials"),
        (403, "InvalidAccessKeyId", "rejected the credentials"),
        (403, "RequestTimeTooSkewed", "clock"),
        (404, "NoSuchBucket", "no bucket named"),
        (403, "AccessDenied", "not scoped"),
    ];
    for (status, code, phrase) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let store = ObjectStore::start();
        let db = seeded(tmp.path(), &store, &store.url());
        store.fail_with(status, code);

        let out = qp(&db, &store).arg("sync").assert().failure();
        let err = String::from_utf8_lossy(&out.get_output().stderr).to_string();
        assert!(
            err.contains(phrase),
            "`{code}` was flattened into something unactionable: {err}"
        );
    }
}

/// An endpoint nothing is listening on is unreachable, and says so.
#[test]
fn an_endpoint_with_nothing_behind_it_is_reported_as_unreachable() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    // A port bound and immediately released: nothing is listening there.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let db = seeded(tmp.path(), &store, &format!("s3://{dead}/{BUCKET}"));

    let out = qp(&db, &store).arg("sync").assert().failure();
    let err = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(
        err.contains("could not reach the object store"),
        "a dead endpoint was not reported as unreachable: {err}"
    );
}

/// A ledger url that is not an object-store url is refused at configuration time.
#[test]
fn only_an_object_store_url_can_be_a_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();
    let dir = tmp.path().join("s");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("db.sqlite");
    qp(&db, &store).arg("init").assert().success();

    for bad in [
        "ssh://host.example/srv/ledger.git",
        "https://host.example/ledger.git",
        "/srv/git/ledger.git",
        "s3://host.example",
    ] {
        let out = qp(&db, &store)
            .args(["remote", "set", "--ledger", bad])
            .assert()
            .failure();
        let err = String::from_utf8_lossy(&out.get_output().stderr).to_string();
        assert!(
            err.contains("s3://<host>/<bucket>"),
            "`{bad}` was refused without naming the url form: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// the plaintext escape hatch is gated to loopback
// ---------------------------------------------------------------------------

/// `QUIPU_S3_PLAINTEXT` must not be able to downgrade a non-loopback endpoint.
///
/// The variable exists only because a `TcpListener` stub cannot terminate TLS.
/// Left host-agnostic it would be a switch that travels — exported once in a
/// shell profile, or inherited by an agent — and because quipu presigns, one
/// cleartext request hands an eavesdropper a url replayable for the whole
/// signature lifetime with the key's full rights. So the gate needs BOTH the
/// variable and a loopback host.
///
/// `0.0.0.0` is what makes this observable rather than argued: the kernel routes
/// it to localhost, so the stub really is on the other end and would record
/// anything sent in the clear — but it is not a loopback literal, so the gate
/// must refuse it. A host that simply did not exist would prove nothing, since
/// a connection that never happens leaks nothing either way.
#[test]
fn the_plaintext_variable_cannot_downgrade_a_non_loopback_endpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::start();

    // Precondition, asserted rather than assumed: this address reaches the
    // stub. Without it the test below would pass vacuously on any platform
    // where `0.0.0.0` is simply unroutable.
    let unspecified = format!("0.0.0.0:{}", store.port());
    std::net::TcpStream::connect(&unspecified).unwrap_or_else(|e| {
        panic!("{unspecified} does not reach the stub ({e}); this test cannot observe what it claims to")
    });

    let db = seeded(tmp.path(), &store, &store.url_at_host("0.0.0.0"));
    store.forget_requests();
    qp(&db, &store).arg("sync").assert().failure();

    // The security property, stated directly: nothing signed reached the wire
    // in cleartext. A downgraded request would arrive as a well-formed S3 GET
    // or PUT carrying `X-Amz-Signature`.
    for r in store.requests() {
        assert!(
            !(matches!(r.method.as_str(), "GET" | "PUT") && r.query().contains("X-Amz-Signature")),
            "a presigned request reached the stub in cleartext against a non-loopback \
             host: {} {}",
            r.method,
            r.target
        );
    }

    // Positive control on the same stub, so a broken client cannot make the
    // assertion above pass for the wrong reason: loopback plus the variable
    // still works.
    let ok_db = seeded(&tmp.path().join("loopback"), &store, &store.url());
    qp(&ok_db, &store).arg("sync").assert().success();
    assert!(
        !store.keys().is_empty(),
        "the loopback control published nothing, so the negative case proves nothing"
    );
}
