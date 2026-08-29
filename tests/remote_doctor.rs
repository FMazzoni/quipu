//! `qp remote doctor` — every fault this command claims to tell apart, produced for real.
//!
//! Hermetic, and it has to be: the object store is an in-process
//! `TcpListener` stub and the authority is a bare repository in a temp dir.
//! Nothing here resolves a name, leaves loopback, or reads a credential that
//! belongs to anybody. Every host is RFC 2606 or `127.0.0.1`.
//!
//! The tests are organised by CAUSE rather than by code path, because the whole
//! claim of this command is that two failures which look identical are reported
//! differently. So the load-bearing assertions are the negative ones: a missing
//! bucket must NOT be reported as a policy problem, a skewed clock must NOT be
//! reported as a bad key, and an authority that answers `ls-remote` must NOT
//! pass a check about writing. Each of those pairs is one mutation away from
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
        !r.has_target("ledger") && !r.has_target("authority"),
        "an unconfigured store must not be probed: {}",
        r.text
    );
}

#[test]
fn a_ledger_only_store_checks_the_ledger_and_says_the_authority_is_absent() {
    let store = ObjectStore::start();
    let s = Store::new();
    s.set_ledger(&store.url());
    let r = s.doctor_with(&store);
    assert_eq!(r.code, 0, "a healthy ledger-only store is fine: {}", r.text);
    assert!(r.has_target("ledger"));
    assert!(!r.has_target("authority"));
    assert!(r.detail("config", "configured").contains("ledger only"));
}

#[test]
fn an_authority_only_store_checks_the_authority_and_says_the_ledger_is_absent() {
    let s = Store::new();
    s.set_authority(&s.bare_repo("authority"));
    let r = s.doctor();
    assert_eq!(r.code, 0, "{}", r.text);
    assert!(r.has_target("authority"));
    assert!(!r.has_target("ledger"));
    assert!(r.detail("config", "configured").contains("authority only"));
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
// the authority
// ---------------------------------------------------------------------------

#[test]
fn a_bare_repository_passes_every_authority_check_and_leaves_no_ref_behind() {
    let s = Store::new();
    let repo = s.bare_repo("authority");
    s.set_authority(&repo);
    let r = s.doctor();
    assert_eq!(r.code, 0, "{}", r.text);
    assert_eq!(r.status("authority", "reach"), "pass");
    assert_eq!(r.status("authority", "write"), "pass");
    assert!(
        refs_of(&repo).is_empty(),
        "the write probe left a ref behind: {:?}",
        refs_of(&repo)
    );
}

#[test]
fn an_authority_that_answers_reads_but_refuses_a_ref_fails_the_write_check() {
    let s = Store::new();
    let repo = s.bare_repo("authority");
    // A repository that serves reads and rejects every push: exactly what a
    // read-only account looks like, and exactly what `ls-remote` cannot see.
    reject_pushes(&repo);
    s.set_authority(&repo);
    let r = s.doctor();
    assert_eq!(r.code, 5);
    assert_eq!(
        r.status("authority", "reach"),
        "pass",
        "reading is fine here, and that is the trap: {}",
        r.text
    );
    assert_eq!(r.status("authority", "write"), "fail");
    let fix = r.remedy("authority", "write");
    assert!(fix.contains("read-only"), "{fix}");
    assert!(fix.contains("CREATION"), "{fix}");
}

#[test]
fn a_local_repository_that_is_not_there_names_ownership_and_absence() {
    let s = Store::new();
    s.set_authority(&format!("{}/nothing-here.git", s.dir().display()));
    let r = s.doctor();
    assert_eq!(r.code, 5);
    assert_eq!(r.status("authority", "reach"), "fail");
    let fix = r.remedy("authority", "reach");
    // The failure that sent someone looking for a missing repository that was
    // present the whole time.
    assert!(fix.contains("OWNERSHIP"), "{fix}");
    assert!(fix.contains("chmod does nothing"), "{fix}");
    assert!(fix.contains("BARE"), "{fix}");
    // A filesystem path has no daemon, no login session and no credential, so
    // the remedy must not send anyone to look at any of them.
    assert!(fix.contains("filesystem path"), "{fix}");
    for word in [
        "authentication",
        "credential",
        "authorized_keys",
        "non-interactive",
    ] {
        assert!(
            !fix.to_lowercase().contains(word),
            "a local path has no `{word}` to be wrong: {fix}"
        );
    }
    assert_eq!(r.status("authority", "write"), "skipped");
}

#[test]
fn a_refused_connection_names_the_service_the_address_and_the_network_lockout() {
    let port = free_port();
    let s = Store::new();
    s.set_authority(&format!("git://127.0.0.1:{port}/authority.git"));
    let r = s.doctor();
    assert_eq!(r.code, 5);
    assert_eq!(r.status("authority", "reach"), "fail");
    let fix = r.remedy("authority", "reach");
    assert!(fix.contains("not running"), "{fix}");
    assert!(fix.contains("stale"), "{fix}");
    assert!(fix.contains("quarantined"), "{fix}");
    assert!(
        !fix.contains("OWNERSHIP"),
        "a refused connection is not a repository problem: {fix}"
    );
}

#[test]
fn an_authority_that_accepts_and_drops_the_connection_points_at_the_server() {
    let hangup = Hangup::start();
    let s = Store::new();
    s.set_authority(&format!("git://127.0.0.1:{}/authority.git", hangup.port));
    let r = s.doctor();
    assert_eq!(r.code, 5);
    let fix = r.remedy("authority", "reach");
    // git prints its catch-all sentence here — the SAME text a `git://` daemon
    // prints when it refuses a repository — so the daemon dying must be named
    // as one of the causes rather than dropped in favour of the other reading.
    assert!(
        fix.contains("SERVER's log"),
        "a daemon that answers and dies is a server-side problem, and this transport's \
         message cannot rule it out: {fix}"
    );
    // And still not a credential problem: `git://` has no login to get wrong.
    for word in ["authentication", "credential", "authorized_keys"] {
        assert!(
            !fix.to_lowercase().contains(word),
            "`git://` has no login, yet the remedy says `{word}`: {fix}"
        );
    }
}

/// The exact failure a real `git://` authority produced, reproduced on loopback.
///
/// A git daemon says "access denied or repository not exported" when a
/// repository is absent, unexported, or owned by a different user than the
/// daemon process. There is no key, no user and no secret anywhere in a
/// `git://` request, so a diagnosis naming a credential is confidently pointing
/// at a subsystem that took no part in the failure — and sends the reader to
/// inspect an ssh key while the repository sits there owned by the wrong user.
#[test]
fn a_git_daemon_refusing_a_repository_is_never_diagnosed_as_authentication() {
    let daemon = ErrDaemon::start("ERR access denied or repository not exported: /authority.git");
    let s = Store::new();
    s.set_authority(&format!("git://127.0.0.1:{}/authority.git", daemon.port));
    let r = s.doctor();
    assert_eq!(r.code, 5);
    assert_eq!(r.status("authority", "reach"), "fail");
    let whole = format!(
        "{} {}",
        r.detail("authority", "reach"),
        r.remedy("authority", "reach")
    )
    .to_lowercase();
    for word in [
        "authentication",
        "credential",
        "authorized_keys",
        "password",
        "token",
    ] {
        assert!(
            !whole.contains(word),
            "`git://` carries no login, yet the diagnosis says `{word}`:\n{whole}"
        );
    }
    let fix = r.remedy("authority", "reach");
    // The three causes this sentence actually means on this transport.
    assert!(fix.contains("OWNERSHIP"), "{fix}");
    assert!(fix.contains("chmod does nothing"), "{fix}");
    assert!(fix.contains("git-daemon-export-ok"), "{fix}");
    assert!(fix.contains("never created"), "{fix}");
}

/// A machine that forbids the git protocol is diagnosed as local policy.
///
/// Nothing is contacted in this case, so the far end may be perfectly healthy
/// and every network-shaped remedy would be a wild goose chase. The environment
/// is what changes the answer here — not the git version and not the far end —
/// which is exactly why the other `git://` tests state the policy rather than
/// inherit it.
#[test]
fn an_environment_that_forbids_the_git_protocol_is_named_as_local_policy() {
    let port = free_port();
    let s = Store::new();
    s.set_authority(&format!("git://127.0.0.1:{port}/authority.git"));
    // Every protocol EXCEPT the one the url names.
    let r = s.doctor_with_env(&[("GIT_ALLOW_PROTOCOL", "file:ssh:http:https")]);
    assert_eq!(r.code, 5);
    let fix = r.remedy("authority", "reach");
    assert!(fix.contains("GIT_ALLOW_PROTOCOL"), "{fix}");
    assert!(
        fix.contains("nothing was contacted"),
        "the far end took no part in this and the remedy must say so: {fix}"
    );
    // The port is closed, so a classifier that ignored the policy message would
    // reach for the network causes instead. It must not.
    assert!(
        !fix.contains("not running"),
        "local policy must not be reported as the service being down: {fix}"
    );
}

/// A port serving something that is not git gets its own answer.
///
/// Neither reachability nor permissions: something IS listening and talking,
/// and it does not speak the git protocol. Before this was named, it fell
/// through to a message that told the reader nothing.
#[test]
fn a_far_end_that_does_not_speak_git_is_named_as_such() {
    let daemon = ErrDaemon::raw("HTTP/1.1 200 OK\r\n\r\n");
    let s = Store::new();
    s.set_authority(&format!("git://127.0.0.1:{}/authority.git", daemon.port));
    let r = s.doctor();
    assert_eq!(r.code, 5);
    let fix = r.remedy("authority", "reach");
    assert!(fix.contains("does not speak the git protocol"), "{fix}");
    assert!(!fix.contains("OWNERSHIP"), "{fix}");
    assert!(!fix.contains("not running"), "{fix}");
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
    s.set_authority(&s.bare_repo("authority"));
    let r = s.doctor_with(&store);
    assert_eq!(r.code, 5);
    assert_eq!(r.json["ok"], Value::from(false));
    assert_eq!(r.json["local_only"], Value::from(false));
    let checks = r.json["checks"].as_array().unwrap();
    assert!(checks.iter().any(|c| c["target"] == "ledger"));
    assert!(checks.iter().any(|c| c["target"] == "authority"));
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
    s.set_authority(&format!("{}/nothing-here.git", s.dir().display()));
    let r = s.doctor();
    assert!(r.text.contains("FAIL  reach"), "{}", r.text);
    assert!(r.text.contains("→"), "{}", r.text);
    assert!(r.text.contains("FAULTS FOUND"), "{}", r.text);
}

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

/// A store in its own temp dir, with the ambient environment scrubbed.
struct Store {
    tmp: tempfile::TempDir,
    db: std::path::PathBuf,
}

impl Store {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let s = Self { tmp, db };
        s.qp().arg("init").assert().success();
        s
    }

    fn dir(&self) -> &std::path::Path {
        self.tmp.path()
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
            "GIT_SSH_COMMAND",
            "GIT_SSH",
        ] {
            c.env_remove(k);
        }
        // Pin what git is allowed to speak, rather than inheriting whatever the
        // machine happens to permit. Some environments disable the plain git
        // protocol, and a test that measures a `git://` DIAGNOSIS gets a
        // different one there — `fatal: transport 'git' not allowed`, which is
        // correct behaviour and the wrong subject. Stating the policy makes
        // these tests measure the same thing on every machine; the one test
        // that covers a hostile policy sets its own.
        c.env("GIT_ALLOW_PROTOCOL", "file:git:ssh:http:https");
        c
    }

    fn set_ledger(&self, url: &str) {
        self.qp()
            .args(["remote", "set", "--ledger", url])
            .assert()
            .success();
    }

    fn set_authority(&self, url: &str) {
        self.qp()
            .args(["remote", "set", "--authority", url])
            .assert()
            .success();
    }

    fn bare_repo(&self, name: &str) -> String {
        let path = self.tmp.path().join(format!("{name}.git"));
        let ok = std::process::Command::new("git")
            .args(["init", "--bare", "-q"])
            .arg(&path)
            .status()
            .expect("spawn git")
            .success();
        assert!(ok, "git init --bare failed");
        path.to_str().unwrap().to_string()
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

/// A listener that accepts a connection and closes it without a word.
///
/// This is a git daemon that cannot run its own helpers: it answers, so the
/// host is plainly up, and the client sees only a reset.
struct Hangup {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    addr: std::net::SocketAddr,
}

impl Hangup {
    fn start() -> Self {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = stop.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                if s.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                if let Ok(mut c) = c {
                    // Read the request first. Closing before the client has
                    // written turns this into a connection RESET, which git
                    // words differently — a race that would make the test
                    // measure whichever of the two shapes won. Both are
                    // diagnosed identically, deliberately, but the test should
                    // still exercise one of them on purpose.
                    use std::io::Read;
                    let mut buf = [0u8; 4096];
                    let _ = c.read(&mut buf);
                    let _ = c.shutdown(std::net::Shutdown::Both);
                    drop(c);
                } else {
                    return;
                }
            }
        });
        Self {
            port: addr.port(),
            stop,
            addr,
        }
    }
}

impl Drop for Hangup {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = std::net::TcpStream::connect(self.addr);
    }
}

/// A stand-in git daemon that answers one request with a protocol error packet.
///
/// Fifteen lines rather than a real `git daemon`: the git protocol's error
/// reply is a single pkt-line beginning `ERR `, and the client turns it into
/// `fatal: remote error: <message>`. That is enough to reproduce the exact
/// sentence a real daemon produced, on loopback, with no daemon to configure,
/// no repository to own and nothing to leave running.
struct ErrDaemon {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    addr: std::net::SocketAddr,
}

impl ErrDaemon {
    /// Answers with bytes that are not the git protocol at all.
    ///
    /// What a port serving something else — a web server, a proxy, a TLS
    /// endpoint — looks like to a git client.
    fn raw(bytes: &str) -> Self {
        Self::serving(bytes.to_string())
    }

    /// A git protocol error packet: length-prefixed, four hex digits counting the prefix.
    fn start(message: &str) -> Self {
        Self::serving(format!("{:04x}{message}", message.len() + 4))
    }

    fn serving(packet: String) -> Self {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = stop.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                if s.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                match c {
                    Ok(mut c) => {
                        let mut buf = [0u8; 4096];
                        let _ = c.read(&mut buf);
                        let _ = c.write_all(packet.as_bytes());
                    }
                    Err(_) => return,
                }
            }
        });
        Self {
            port: addr.port(),
            stop,
            addr,
        }
    }
}

impl Drop for ErrDaemon {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = std::net::TcpStream::connect(self.addr);
    }
}

/// Makes a bare repository serve reads and refuse every push.
///
/// A `pre-receive` hook rather than file permissions: permissions do nothing
/// when the suite runs as root, which is how CI containers usually run, and a
/// check that quietly stops checking is worse than no check.
fn reject_pushes(repo: &str) {
    let hook = std::path::Path::new(repo).join("hooks").join("pre-receive");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\necho 'read-only mirror' >&2\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Every ref a repository holds, so a probe that failed to clean up is visible.
fn refs_of(repo: &str) -> Vec<String> {
    let out = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(repo)
        .args(["for-each-ref", "--format=%(refname)"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}
