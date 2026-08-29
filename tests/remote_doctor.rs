//! `qp remote doctor` — every fault this command claims to tell apart, produced for real.
//!
//! Hermetic, and it has to be: the object store is an in-process
//! `TcpListener` stub. Nothing here resolves a name, leaves loopback, or reads
//! a credential that belongs to anybody. Every host is RFC 2606 or
//! `127.0.0.1`.
//!
//! The front-door tests hold the same line: a real `qp serve` on an ephemeral
//! loopback port for the cases that need a real one, and an in-process stub
//! `TcpListener` for the health bodies a real server cannot be made to send.
//! A "server that is not listening" is a port nothing is bound to, never a host.
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
// the front door: the three faults that used to be one
// ---------------------------------------------------------------------------

#[test]
fn a_reachable_server_that_serves_this_project_is_clean() {
    let s = Store::named("alpha");
    let srv = Serve::start(&s.db);
    s.set_server(&srv.url);
    let r = s.doctor();
    assert_eq!(r.code, 0, "a healthy front door is not a fault: {}", r.text);
    assert_eq!(r.status("server", "url"), "pass");
    assert_eq!(r.status("server", "reachable"), "pass");
    assert_eq!(r.status("server", "serves"), "pass");
    assert!(r.detail("server", "serves").contains("alpha"));
    // Same binary both ends, so the versions agree.
    assert_eq!(r.status("server", "version"), "pass");
    assert!(r.detail("config", "configured").contains("server set"));
}

#[test]
fn a_server_that_is_not_listening_is_the_unreachable_fault() {
    let s = Store::named("alpha");
    s.set_server(&format!("http://127.0.0.1:{}", free_port()));
    let r = s.doctor();
    assert_eq!(r.code, 5, "{}", r.text);
    assert_eq!(r.status("server", "reachable"), "fail");
    let fix = r.remedy("server", "reachable");
    assert!(
        fix.contains("qp serve"),
        "the remedy must say what to start: {fix}"
    );
    assert!(
        fix.contains("qp remote clear --front-door"),
        "and how to get back to the local store: {fix}"
    );
    // Nothing after it may claim to know anything.
    for later in ["version", "serves"] {
        assert_eq!(r.status("server", later), "skipped");
    }
}

/// The failure this slice exists to end: alive is not the same as serving you.
///
/// The two want opposite fixes — start a process, versus restart one with a
/// different `--project` set — so the load-bearing assertions are the negative
/// ones. `reachable` must PASS, and the remedy must not send anyone to start a
/// server that is already running.
#[test]
fn a_live_server_that_does_not_serve_this_project_is_a_different_fault() {
    let other = Store::named("beta");
    let srv = Serve::start(&other.db);
    let s = Store::named("alpha");
    s.set_server(&srv.url);
    let r = s.doctor();
    assert_eq!(r.code, 5, "{}", r.text);
    assert_eq!(
        r.status("server", "reachable"),
        "pass",
        "a server that answered is reachable; saying otherwise is the confident wrong answer"
    );
    assert_eq!(r.status("server", "serves"), "fail");
    let d = r.detail("server", "serves");
    assert!(d.contains("alpha") && d.contains("beta"), "{d}");
    let fix = r.remedy("server", "serves");
    assert!(fix.contains("--project"), "{fix}");
    assert!(
        !fix.contains("cannot reach") && !fix.contains("network"),
        "an answering server must not be diagnosed as an absent one: {fix}"
    );
}

/// Something else on the port is not a quipu front door, and is not silence either.
#[test]
fn a_port_held_by_something_else_is_not_reported_as_unreachable() {
    let stub = HealthStub::start("200 OK", "hello from some other service");
    let s = Store::named("alpha");
    s.set_server(&stub.url);
    let r = s.doctor();
    assert_eq!(r.code, 5, "{}", r.text);
    assert_eq!(r.status("server", "reachable"), "fail");
    let d = r.detail("server", "reachable");
    assert!(
        d.contains("not a quipu front door"),
        "something answered, and the report must say so: {d}"
    );
    assert!(
        !r.remedy("server", "reachable").contains("qp serve --bind"),
        "starting a second server on a taken port is the wrong remedy"
    );
}

/// A version difference is reported and is NOT a fault.
///
/// Built with a stub because the only way a real `qp serve` reports a different
/// version is by being a different build, which a test cannot arrange.
#[test]
fn a_version_difference_is_reported_without_failing_the_run() {
    let stub = HealthStub::start(
        "200 OK",
        r#"{"version":"0.0.1-elsewhere","bind":"127.0.0.1:0","uptime_s":1,"projects":["alpha"]}"#,
    );
    let s = Store::named("alpha");
    s.set_server(&stub.url);
    let r = s.doctor();
    assert_eq!(
        r.code, 0,
        "refusing to work over a version difference is worse than the problem: {}",
        r.text
    );
    assert_eq!(r.status("server", "version"), "note");
    assert!(r.detail("server", "version").contains("0.0.1-elsewhere"));
    assert!(r.remedy("server", "version").contains("not a fault"));
    assert_eq!(r.status("server", "serves"), "pass");
}

/// A url of the ledger's kind in the server slot, and vice versa.
#[test]
fn each_url_kind_is_refused_in_the_other_slot() {
    let s = Store::named("alpha");
    s.qp()
        .args(["remote", "set", "--front-door", "s3://host.invalid/bucket"])
        .assert()
        .failure();
    s.qp()
        .args(["remote", "set", "--ledger", "https://qp.example.com"])
        .assert()
        .failure();
    // And the shallow check still runs first. Written with `=` so clap hands
    // the flag-shaped text through as a VALUE — the point is that quipu refuses
    // it, not that clap does.
    s.qp()
        .args(["remote", "set", "--front-door=--upload-pack=evil"])
        .assert()
        .failure();
}

// ---------------------------------------------------------------------------
// the four combinations of what is attached
// ---------------------------------------------------------------------------

/// Both configured: both sequences run, and neither hides the other.
#[test]
fn a_ledger_and_a_server_are_both_checked_and_neither_hides_the_other() {
    let store = ObjectStore::start();
    let s = Store::named("alpha");
    s.set_ledger(&store.url());
    // A front door that is not there, so the SERVER sequence fails while the
    // ledger sequence passes. The failure must be attributed to the server.
    s.set_server(&format!("http://127.0.0.1:{}", free_port()));
    let r = s.doctor_with(&store);
    assert_eq!(r.code, 5, "{}", r.text);
    assert_eq!(r.status("ledger", "get"), "pass", "{}", r.text);
    assert_eq!(r.status("server", "reachable"), "fail");
    assert!(r
        .detail("config", "configured")
        .contains("ledger and server"));

    // Now the other way round: a broken ledger must not suppress a healthy
    // front door's checks.
    let srv = Serve::start(&s.db);
    s.set_server(&srv.url);
    let broken = Store::named("alpha2");
    broken.set_ledger(&format!("s3://127.0.0.1:{}/quipu-ledger", free_port()));
    broken.set_server(&srv.url);
    let r = broken.doctor_with_env(&[
        ("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE"),
        (
            "AWS_SECRET_ACCESS_KEY",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        ),
        ("QUIPU_S3_PLAINTEXT", "1"),
    ]);
    assert_eq!(r.code, 5);
    assert_eq!(r.status("ledger", "endpoint"), "fail");
    assert_eq!(
        r.status("server", "reachable"),
        "pass",
        "a dead ledger must not make the front door look dead: {}",
        r.text
    );
}

/// Local-only stays a first-class healthy answer now that there are two urls.
#[test]
fn a_store_with_neither_url_is_still_healthy_and_probes_nothing() {
    let s = Store::named("alpha");
    let r = s.doctor();
    assert_eq!(r.code, 0, "{}", r.text);
    assert_eq!(r.status("config", "configured"), "note");
    assert!(r.detail("config", "configured").contains("local-only"));
    assert!(!r.has_target("ledger"));
    assert!(!r.has_target("server"), "{}", r.text);
    assert_eq!(r.json["local_only"], Value::from(true));
}

/// A server-only store is attached, so it is not local-only.
#[test]
fn a_server_only_store_checks_the_server_and_nothing_else() {
    let s = Store::named("alpha");
    let srv = Serve::start(&s.db);
    s.set_server(&srv.url);
    let r = s.doctor();
    assert_eq!(r.code, 0, "{}", r.text);
    assert_eq!(r.json["local_only"], Value::from(false));
    assert!(!r.has_target("ledger"), "{}", r.text);
    assert!(r.has_target("server"));
}

/// Clearing takes the store back to local-only, one url at a time.
#[test]
fn clearing_one_url_leaves_the_other_attached() {
    let store = ObjectStore::start();
    let s = Store::named("alpha");
    s.set_ledger(&store.url());
    s.set_server("https://qp.example.com");
    s.qp()
        .args(["remote", "clear", "--front-door"])
        .assert()
        .success();
    let r = s.doctor_with(&store);
    assert!(!r.has_target("server"), "{}", r.text);
    assert!(r.has_target("ledger"));
    s.qp().args(["remote", "clear"]).assert().success();
    let r = s.doctor();
    assert_eq!(r.json["local_only"], Value::from(true), "{}", r.text);
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
        Self::init(&[])
    }

    /// A store whose project name is chosen rather than derived from a temp dir.
    ///
    /// The front-door tests compare the store's name against the list a server
    /// reports, so the name has to be one the test can write down.
    fn named(name: &str) -> Self {
        Self::init(&["--name", name])
    }

    fn init(extra: &[&str]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let s = Self { _tmp: tmp, db };
        s.qp().arg("init").args(extra).assert().success();
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

    fn set_server(&self, url: &str) {
        self.qp()
            .args(["remote", "set", "--front-door", url])
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

/// A real `qp serve` on an ephemeral loopback port, killed when the test ends.
///
/// A real process rather than a stub for the healthy cases, because the claim
/// under test is about what `/healthz` actually answers — a stub of it would
/// only test the test.
struct Serve {
    child: std::process::Child,
    url: String,
}

impl Serve {
    fn start(db: &std::path::Path) -> Self {
        let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("qp"))
            .env("QP_DB", db)
            .env("QUIPU_REGISTRY", db.parent().unwrap().join("registry.json"))
            .args(["serve", "--bind", "127.0.0.1:0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn qp serve");
        let mut reader = std::io::BufReader::new(child.stderr.take().expect("piped stderr"));
        let bind = loop {
            let mut line = String::new();
            let n = std::io::BufRead::read_line(&mut reader, &mut line).expect("read stderr");
            assert!(n > 0, "qp serve exited before logging startup");
            if let Ok(v) = serde_json::from_str::<Value>(line.trim_end()) {
                if v["event"] == "startup" {
                    break v["bind"].as_str().expect("bind address").to_string();
                }
            }
        };
        // Drained on a thread: a full stderr pipe blocks the process under test.
        std::thread::spawn(
            move || {
                for _ in std::io::BufRead::lines(reader).map_while(Result::ok) {}
            },
        );
        Serve {
            child,
            url: format!("http://{bind}"),
        }
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A loopback listener that answers every request with one fixed response.
///
/// For the health bodies a real `qp serve` cannot be made to send: a different
/// version string, and a 200 that is not a health payload at all.
struct HealthStub {
    url: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl HealthStub {
    fn start(status: &str, body: &str) -> Self {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&stop);
        std::thread::spawn(move || {
            use std::io::Write;
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                match l.accept() {
                    Ok((mut sock, _)) => {
                        // The request is read only far enough to unblock the
                        // client; the answer does not depend on it.
                        let mut buf = [0u8; 1024];
                        let _ = std::io::Read::read(&mut sock, &mut buf);
                        let _ = sock.write_all(resp.as_bytes());
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                }
            }
        });
        HealthStub { url, stop }
    }
}

impl Drop for HealthStub {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A port nothing is listening on, for the refused-connection cases.
///
/// Bound and released, so the number is real and free rather than guessed.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}
