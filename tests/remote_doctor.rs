//! `qp remote doctor` — every fault this command claims to tell apart, produced for real.
//!
//! Hermetic, and it has to be: the object store is an in-process
//! `TcpListener` stub. Nothing here resolves a name, leaves loopback, or reads
//! a credential that belongs to anybody. Every host is RFC 2606 or
//! `127.0.0.1`.
//!
//! The tests are organised by CAUSE rather than by code path, because the whole
//! claim of this command is that two failures which look identical are reported
//! differently. So the load-bearing assertions are the negative ones: a missing
//! bucket must NOT be reported as a policy problem, and a skewed clock must NOT
//! be reported as a bad key. Each of those pairs is one mutation away from
//! collapsing, and a test that only asserts the happy phrase would not notice.

mod common;

use assert_cmd::Command;
use common::objstore::ObjectStore;
use serde_json::Value;

/// A ledger with no prefix, and one with, since the prefix is what a scoped key is scoped to.
const PREFIX: &str = "team";

// ---------------------------------------------------------------------------
// what is configured at all
// ---------------------------------------------------------------------------

#[test]
fn a_store_with_nothing_configured_is_reported_and_is_not_a_fault() {
    let s = Store::new();
    let r = s.doctor();
    assert_eq!(r.code, 0, "local-only is a supported state, not a failure");
    assert_eq!(r.status("config", "configured"), "note");
    assert!(r.detail("config", "configured").contains("local-only"));
    // Nothing else ran, because there is nothing to check.
    assert!(
        !r.has_target("ledger"),
        "an unconfigured store must not be probed: {}",
        r.text
    );
}

#[test]
fn a_ledger_store_checks_the_ledger_and_nothing_else() {
    let store = ObjectStore::start();
    let s = Store::new();
    s.set_ledger(&store.url());
    let r = s.doctor_with(&store);
    assert_eq!(r.code, 0, "a healthy ledger store is fine: {}", r.text);
    assert!(r.has_target("ledger"));
    assert!(r.detail("config", "configured").contains("ledger set"));
    // The unconditional sequences still run; nothing else does.
    let targets: std::collections::BTreeSet<String> = r.json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["target"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        targets,
        ["config", "ledger", "project", "registry"]
            .into_iter()
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>(),
        "an unexpected sequence ran: {}",
        r.text
    );
}

// ---------------------------------------------------------------------------
// object storage: the pairs that look alike
// ---------------------------------------------------------------------------

#[test]
fn credentials_absent_names_the_variables_it_looked_for() {
    let store = ObjectStore::start();
    let s = Store::new();
    s.set_ledger(&store.url());
    // Deliberately WITHOUT the store's env: this is the absent case.
    let r = s.doctor();
    assert_eq!(r.code, 5);
    assert_eq!(r.status("ledger", "credentials"), "fail");
    let d = r.detail("ledger", "credentials");
    assert!(d.contains("AWS_ACCESS_KEY_ID"), "{d}");
    assert!(d.contains("AWS_SECRET_ACCESS_KEY"), "{d}");
    // And it stopped: a missing credential makes every later answer meaningless.
    for later in [
        "endpoint",
        "clock",
        "signature",
        "bucket",
        "list",
        "put",
        "get",
    ] {
        assert_eq!(
            r.status("ledger", later),
            "skipped",
            "`{later}` ran after credentials were missing, which can only mislead"
        );
    }
}

#[test]
fn credentials_rejected_is_told_apart_from_credentials_absent() {
    let store = ObjectStore::start();
    store.fail_with(403, "InvalidAccessKeyId");
    let s = Store::new();
    s.set_ledger(&store.url());
    let r = s.doctor_with(&store);
    assert_eq!(r.code, 5);
    // Present, and rejected. Two different checks, two different fixes.
    assert_eq!(r.status("ledger", "credentials"), "pass");
    assert_eq!(r.status("ledger", "signature"), "fail");
    let fix = r.remedy("ledger", "signature");
    assert!(
        fix.contains("wrong") && !fix.contains("export"),
        "a rejected key must not be diagnosed as a missing one: {fix}"
    );
}

#[test]
fn an_unreachable_endpoint_stops_before_anything_it_would_have_to_guess_at() {
    let port = free_port();
    let s = Store::new();
    s.set_ledger(&format!("s3://127.0.0.1:{port}/quipu-ledger"));
    let r = s.doctor_with_env(&[
        ("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE"),
        (
            "AWS_SECRET_ACCESS_KEY",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        ),
        ("QUIPU_S3_PLAINTEXT", "1"),
    ]);
    assert_eq!(r.code, 5);
    assert_eq!(r.status("ledger", "credentials"), "pass");
    assert_eq!(r.status("ledger", "endpoint"), "fail");
    assert_eq!(r.status("ledger", "signature"), "skipped");
    let fix = r.remedy("ledger", "endpoint");
    // Three causes, all named, because this probe genuinely cannot separate them.
    assert!(fix.contains("down"), "{fix}");
    assert!(fix.contains("stale"), "{fix}");
    assert!(fix.contains("quarantined"), "{fix}");
}

#[test]
fn a_missing_bucket_is_not_reported_as_a_policy_problem() {
    let store = ObjectStore::start();
    let s = Store::new();
    // A bucket the stub does not serve: it answers NoSuchBucket by name.
    s.set_ledger(&format!("s3://127.0.0.1:{}/no-such-bucket", store.port()));
    let r = s.doctor_with(&store);
    assert_eq!(r.code, 5);
    assert_eq!(r.status("ledger", "signature"), "pass");
    assert_eq!(r.status("ledger", "bucket"), "fail");
    let fix = r.remedy("ledger", "bucket");
    assert!(fix.contains("create"), "{fix}");
    assert!(
        !fix.to_lowercase().contains("policy"),
        "a missing bucket sent the reader to fix a policy: {fix}"
    );
    assert_eq!(r.status("ledger", "list"), "skipped");
}

#[test]
fn a_key_that_cannot_list_its_prefix_is_not_reported_as_a_missing_bucket() {
    let store = ObjectStore::start();
    // Exactly a prefix-scoped key: the root answers, the prefix does not.
    store.deny_prefix(PREFIX);
    let s = Store::new();
    s.set_ledger(&store.url_with_prefix(PREFIX));
    let r = s.doctor_with(&store);
    assert_eq!(r.code, 5);
    assert_eq!(
        r.status("ledger", "bucket"),
        "pass",
        "the bucket demonstrably exists — its root listed: {}",
        r.text
    );
    assert_eq!(r.status("ledger", "list"), "fail");
    let fix = r.remedy("ledger", "list");
    assert!(fix.contains("ListBucket"), "{fix}");
    assert!(
        fix.contains("already exists"),
        "the fix here is the policy, and the reader must be told NOT to create a bucket: {fix}"
    );
}

#[test]
fn a_denial_that_cannot_be_narrowed_names_both_causes_instead_of_choosing() {
    let store = ObjectStore::start();
    // Denied everywhere, root included — which is what a real store does for a
    // bucket you may not see, whether or not it is there.
    store.fail_with(403, "AccessDenied");
    let s = Store::new();
    s.set_ledger(&store.url_with_prefix(PREFIX));
    let r = s.doctor_with(&store);
    assert_eq!(r.code, 5);
    assert_eq!(
        r.status("ledger", "bucket"),
        "note",
        "an undeterminable bucket must not be asserted either way: {}",
        r.text
    );
    let fix = r.remedy("ledger", "list");
    assert!(fix.contains("TWO possibilities"), "{fix}");
    assert!(fix.contains("does not exist"), "{fix}");
    assert!(fix.contains("not scoped"), "{fix}");
}

#[test]
fn a_skewed_clock_is_named_as_the_clock_rather_than_the_credentials() {
    let store = ObjectStore::start();
    // The store still SERVES: only its Date header betrays the difference.
    store.skew_clock(40 * 60);
    let s = Store::new();
    s.set_ledger(&store.url());
    let r = s.doctor_with(&store);
    assert_eq!(r.code, 5);
    assert_eq!(r.status("ledger", "clock"), "fail");
    let fix = r.remedy("ledger", "clock");
    assert!(fix.contains("CLOCK"), "{fix}");
    assert!(
        fix.contains("Do not rotate"),
        "the whole point is to stop someone rotating a good key: {fix}"
    );
    assert_eq!(
        r.status("ledger", "signature"),
        "skipped",
        "the credential verdict after a bad clock is exactly the misleading answer"
    );
}

#[test]
fn a_store_that_refuses_the_timestamp_outright_is_also_the_clock() {
    let store = ObjectStore::start();
    store.fail_with(403, "RequestTimeTooSkewed");
    let s = Store::new();
    s.set_ledger(&store.url());
    let r = s.doctor_with(&store);
    assert_eq!(r.code, 5);
    assert_eq!(
        r.status("ledger", "clock"),
        "fail",
        "a skew refusal reported as a credential failure is the afternoon this command exists to save: {}",
        r.text
    );
    assert_ne!(r.status("ledger", "signature"), "fail");
}

#[test]
fn a_clock_inside_the_tolerance_passes() {
    let store = ObjectStore::start();
    store.skew_clock(30);
    let s = Store::new();
    s.set_ledger(&store.url());
    let r = s.doctor_with(&store);
    assert_eq!(r.status("ledger", "clock"), "pass", "{}", r.text);
    assert_eq!(r.code, 0);
}

#[test]
fn a_key_that_can_list_but_not_write_fails_the_put_and_not_the_list() {
    let store = ObjectStore::start();
    store.deny_method("PUT");
    let s = Store::new();
    s.set_ledger(&store.url());
    let r = s.doctor_with(&store);
    assert_eq!(r.code, 5);
    assert_eq!(r.status("ledger", "list"), "pass");
    assert_eq!(r.status("ledger", "put"), "fail");
    let fix = r.remedy("ledger", "put");
    assert!(fix.contains("PutObject"), "{fix}");
    assert_eq!(r.status("ledger", "get"), "skipped");
}

#[test]
fn a_healthy_ledger_passes_list_put_and_get_and_leaves_one_flat_probe_key() {
    let store = ObjectStore::start();
    let s = Store::new();
    s.set_ledger(&store.url_with_prefix(PREFIX));
    let r = s.doctor_with(&store);
    assert_eq!(r.code, 0, "{}", r.text);
    for name in [
        "url",
        "credentials",
        "endpoint",
        "clock",
        "signature",
        "bucket",
        "list",
        "put",
        "get",
    ] {
        let st = r.status("ledger", name);
        assert!(
            st == "pass" || st == "note",
            "`{name}` was {st} on a healthy ledger: {}",
            r.text
        );
    }
    let keys = store.keys();
    assert_eq!(keys.len(), 1, "exactly one probe object: {keys:?}");
    let k = &keys[0];
    assert!(k.starts_with(&format!("{PREFIX}/")), "{k}");
    // Flat under the prefix. A `/` here would make the probe look like a
    // PROJECT to `list_project_keys`, which reads CommonPrefixes.
    assert_eq!(
        k.matches('/').count(),
        1,
        "the probe key must not add a path segment: {k}"
    );

    // Run it twice: the probe is overwritten, never accumulated, because the
    // object client has no DELETE and needs no delete permission.
    let again = s.doctor_with(&store);
    assert_eq!(again.code, 0);
    assert_eq!(
        store.keys().len(),
        1,
        "the probe accumulated: {:?}",
        store.keys()
    );
}

// ---------------------------------------------------------------------------
// output contract
// ---------------------------------------------------------------------------

#[test]
fn json_carries_every_check_by_name_and_agrees_with_the_exit_code() {
    let store = ObjectStore::start();
    store.fail_with(403, "InvalidAccessKeyId");
    let s = Store::new();
    s.set_ledger(&store.url());
    let r = s.doctor_with(&store);
    assert_eq!(r.code, 5);
    assert_eq!(r.json["ok"], Value::from(false));
    assert_eq!(r.json["local_only"], Value::from(false));
    let checks = r.json["checks"].as_array().unwrap();
    assert!(checks.iter().any(|c| c["target"] == "ledger"));
    // Every failure carries a remedy; that is the contract, not a nicety.
    for c in checks.iter().filter(|c| c["status"] == "fail") {
        assert!(
            c["remedy"].is_string(),
            "a failure with no remedy is a slower error message: {c}"
        );
    }
}

#[test]
fn the_human_form_prints_a_remedy_under_each_failure() {
    let s = Store::new();
    // A url of the right shape pointed at a port nothing is listening on: the
    // failure is real, and it is produced without leaving loopback.
    s.set_ledger(&format!("s3://127.0.0.1:{}/quipu-ledger", free_port()));
    let r = s.doctor_with_env(&[
        ("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE"),
        (
            "AWS_SECRET_ACCESS_KEY",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        ),
        ("QUIPU_S3_PLAINTEXT", "1"),
    ]);
    assert!(r.text.contains("FAIL  endpoint"), "{}", r.text);
    assert!(r.text.contains("→"), "{}", r.text);
    assert!(r.text.contains("FAULTS FOUND"), "{}", r.text);
}

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

/// A store in its own temp dir, with the ambient environment scrubbed.
struct Store {
    /// Held, never read: dropping it deletes the directory the store lives in,
    /// so the binding is the lifetime and the underscore says so.
    _tmp: tempfile::TempDir,
    db: std::path::PathBuf,
}

impl Store {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let s = Self { _tmp: tmp, db };
        s.qp().arg("init").assert().success();
        s
    }

    /// `qp` with every credential and store pointer removed from the environment.
    ///
    /// A developer with real object-store credentials exported must get the
    /// same answer from these tests as CI does, and no test may reach a real
    /// endpoint because one happened to be configured.
    fn qp(&self) -> Command {
        let mut c = Command::cargo_bin("qp").unwrap();
        c.env("QP_DB", &self.db);
        // The doctor reads the project registry, so it must read THIS test's
        // and never the developer's: a duplicate or dangling entry in a real
        // one is a fault, and it would fail these tests for a reason that has
        // nothing to do with remotes.
        c.env(
            "QUIPU_REGISTRY",
            self.db.parent().unwrap().join("registry.json"),
        );
        for k in [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
            "QUIPU_S3_PLAINTEXT",
        ] {
            c.env_remove(k);
        }
        c
    }

    fn set_ledger(&self, url: &str) {
        self.qp()
            .args(["remote", "set", "--ledger", url])
            .assert()
            .success();
    }

    fn doctor(&self) -> Run {
        self.doctor_with_env(&[])
    }

    fn doctor_with(&self, store: &ObjectStore) -> Run {
        let env: Vec<(&str, String)> = store.env();
        let borrowed: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.doctor_with_env(&borrowed)
    }

    fn doctor_with_env(&self, env: &[(&str, &str)]) -> Run {
        let mut human = self.qp();
        let mut json = self.qp();
        for (k, v) in env {
            human.env(k, v);
            json.env(k, v);
        }
        let h = human.args(["remote", "doctor"]).output().unwrap();
        let j = json.args(["remote", "doctor", "--json"]).output().unwrap();
        assert_eq!(
            h.status.code(),
            j.status.code(),
            "the two output formats disagreed about the exit code"
        );
        Run {
            code: h.status.code().unwrap_or(-1),
            text: String::from_utf8_lossy(&h.stdout).to_string(),
            json: serde_json::from_slice(&j.stdout)
                .unwrap_or_else(|e| panic!("doctor --json was not JSON: {e}")),
        }
    }
}

struct Run {
    code: i32,
    text: String,
    json: Value,
}

impl Run {
    fn find(&self, target: &str, name: &str) -> &Value {
        self.json["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .find(|c| c["target"] == target && c["name"] == name)
            .unwrap_or_else(|| panic!("no `{target}/{name}` check in:\n{}", self.text))
    }
    fn status(&self, target: &str, name: &str) -> String {
        self.find(target, name)["status"]
            .as_str()
            .unwrap()
            .to_string()
    }
    fn detail(&self, target: &str, name: &str) -> String {
        self.find(target, name)["detail"]
            .as_str()
            .unwrap()
            .to_string()
    }
    fn remedy(&self, target: &str, name: &str) -> String {
        self.find(target, name)["remedy"]
            .as_str()
            .unwrap_or_else(|| panic!("`{target}/{name}` failed with no remedy:\n{}", self.text))
            .to_string()
    }
    fn has_target(&self, target: &str) -> bool {
        self.json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["target"] == target)
    }
}

/// A port nothing is listening on, for the refused-connection cases.
///
/// Bound and released, so the number is real and free rather than guessed.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}
