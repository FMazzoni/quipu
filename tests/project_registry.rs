//! Naming a store, and the machine-local index that maps a name to it.
//!
//! Every store here is a synthetic one in a temp directory, and every command
//! runs with `QUIPU_REGISTRY` (plus `HOME` and `XDG_CONFIG_HOME`) pointed
//! inside that directory. A test that writes to a real config directory is a
//! defect, not a mess — `the_real_config_directory_is_never_touched` is the
//! assertion that keeps the rest of this file honest.
//!
//! Nothing here reaches a network, and no remote is ever configured: naming is
//! local, and the checks that cover it must run on a store with no ledger at
//! all.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// One machine's worth of state: some stores, and the registry they share.
struct Fixture {
    tmp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// Where the registry is forced to live for this fixture.
    fn registry(&self) -> PathBuf {
        self.root()
            .join("config")
            .join("quipu")
            .join("registry.json")
    }

    /// The store a project of this name would have, at the conventional layout.
    fn store(&self, dir: &str) -> PathBuf {
        self.root().join(dir).join(".quipu").join("db.sqlite")
    }

    fn qp(&self, db: &Path) -> Command {
        let mut c = Command::cargo_bin("qp").unwrap();
        c.env("QP_DB", db);
        c.env("QUIPU_REGISTRY", self.registry());
        // Belt and braces: even if the override were ignored, the XDG lookup
        // must not be able to find the developer's real config directory.
        c.env("HOME", self.root().join("home"));
        c.env("XDG_CONFIG_HOME", self.root().join("xdg"));
        c.env_remove("QUIPU_LEDGER");
        c
    }

    fn ok(&self, db: &Path, args: &[&str]) -> String {
        let out = self
            .qp(db)
            .args(args)
            .assert()
            .try_success()
            .unwrap_or_else(|e| panic!("`qp {}` failed: {e}", args.join(" ")));
        String::from_utf8_lossy(&out.get_output().stdout).to_string()
    }

    fn json(&self, db: &Path, args: &[&str]) -> Value {
        serde_json::from_str(&self.ok(db, args)).expect("json output")
    }

    fn fail(&self, db: &Path, args: &[&str]) -> String {
        let out = self.qp(db).args(args).assert().failure();
        String::from_utf8_lossy(&out.get_output().stderr).to_string()
    }

    /// A store initialised at `<root>/<dir>/.quipu/db.sqlite`.
    fn init(&self, dir: &str, extra: &[&str]) -> PathBuf {
        let db = self.store(dir);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let mut args = vec!["init"];
        args.extend_from_slice(extra);
        self.ok(&db, &args);
        db
    }

    fn meta(&self, db: &Path, key: &str) -> Option<String> {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .ok()
    }

    /// The doctor's verdict for one named check on this store.
    fn check(&self, db: &Path, name: &str) -> Value {
        let out = self
            .qp(db)
            .args(["remote", "doctor", "--json"])
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!("doctor json: {e}\n{}", String::from_utf8_lossy(&out.stdout))
        });
        v["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("no check named `{name}` in {v}"))
            .clone()
    }
}

// ---------------------------------------------------------------------------
// the name, which lives in the store
// ---------------------------------------------------------------------------

#[test]
fn init_names_the_store_after_its_directory() {
    let f = Fixture::new();
    let db = f.init("widget-service", &[]);
    assert_eq!(
        f.meta(&db, "project_name").as_deref(),
        Some("widget-service")
    );
    assert_eq!(
        f.meta(&db, "project_name_origin").as_deref(),
        Some("assigned")
    );
}

#[test]
fn a_directory_name_is_slugged_rather_than_stored_raw() {
    let f = Fixture::new();
    let db = f.init("Widget Service (v2)", &[]);
    assert_eq!(
        f.meta(&db, "project_name").as_deref(),
        Some("widget-service-v2")
    );
}

#[test]
fn the_name_flag_overrides_the_directory() {
    let f = Fixture::new();
    let db = f.store("some-checkout");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let out = f.json(&db, &["init", "--name", "ledger-tools", "--json"]);
    assert_eq!(out["project_name"], "ledger-tools");
    assert_eq!(f.meta(&db, "project_name").as_deref(), Some("ledger-tools"));
}

#[test]
fn a_malformed_name_is_refused() {
    let f = Fixture::new();
    let db = f.store("alpha");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    for bad in [
        "Alpha",
        "with space",
        "-leading",
        "trailing-",
        "under_score",
        "",
    ] {
        // `--name=<v>` rather than two arguments: a value beginning with `-`
        // would otherwise be parsed as a flag, and the test would pass on a
        // clap usage error instead of on the validation it is about.
        let err = f.fail(&db, &["init", &format!("--name={bad}")]);
        assert!(
            err.contains("project name"),
            "`{bad}` should be refused by name validation: {err}"
        );
    }
    assert!(
        f.meta(&db, "project_name").is_none(),
        "a refused init must not have named the store anyway"
    );
}

#[test]
fn an_assigned_name_is_fixed_and_a_rename_is_refused() {
    let f = Fixture::new();
    let db = f.init("alpha", &["--name", "alpha"]);
    // Re-initialising with the same name is idempotent.
    f.ok(&db, &["init", "--name", "alpha"]);
    let err = f.fail(&db, &["init", "--name", "beta"]);
    assert!(err.contains("alpha") && err.contains("beta"), "{err}");
    assert_eq!(f.meta(&db, "project_name").as_deref(), Some("alpha"));
}

/// A store from before names existed must heal, not fail — and the placeholder
/// it heals with must be replaceable, or every such store is stuck with it.
///
/// The fixture rewinds `schema_version` as well as removing the rows: a store
/// stamped at a version this binary already knows is exactly the state a
/// version-gated backfill would never revisit, and the `INSERT OR IGNORE`
/// batch in `db::migrate` ignores per row, so a naive fix there would mint a
/// value and leave the derivation unreachable.
#[test]
fn a_store_that_predates_naming_is_healed_and_can_still_be_named() {
    let f = Fixture::new();
    let db = f.init("alpha", &[]);
    f.ok(&db, &["add", "a ticket"]);
    let uuid = f.meta(&db, "project_uuid").unwrap();
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "DELETE FROM meta WHERE key IN ('project_name','project_name_origin');
             UPDATE meta SET value = '5' WHERE key = 'schema_version';",
        )
        .unwrap();
    }

    // Any command at all heals it.
    f.ok(&db, &["list"]);
    let healed = f.meta(&db, "project_name").unwrap();
    assert_eq!(healed, format!("qp-{}", &uuid[..8]));
    assert_eq!(
        f.meta(&db, "project_name_origin").as_deref(),
        Some("derived")
    );
    // Deterministic: healing twice cannot produce two names.
    f.ok(&db, &["list"]);
    assert_eq!(f.meta(&db, "project_name").unwrap(), healed);

    // And the placeholder is not a life sentence — naming it is the migration
    // finishing, not a rename.
    f.ok(&db, &["init", "--name", "alpha"]);
    assert_eq!(f.meta(&db, "project_name").as_deref(), Some("alpha"));
    assert_eq!(
        f.meta(&db, "project_name_origin").as_deref(),
        Some("assigned")
    );
}

// ---------------------------------------------------------------------------
// the registry
// ---------------------------------------------------------------------------

#[test]
fn register_list_and_lookup_round_trip() {
    let f = Fixture::new();
    let a = f.init("alpha", &[]);
    let b = f.init("beta", &[]);
    assert_eq!(
        f.json(&a, &["project", "register", "--json"])["result"],
        "added"
    );
    assert_eq!(
        f.json(&a, &["project", "register", "--json"])["result"],
        "unchanged",
        "registering the same store twice must not churn the file"
    );
    f.ok(&b, &["project", "register"]);

    let listed = f.json(&a, &["project", "list", "--json"]);
    let names: Vec<&str> = listed["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["alpha", "beta"]);

    let found = f.json(&a, &["project", "lookup", "beta", "--json"]);
    assert_eq!(
        Path::new(found["path"].as_str().unwrap()),
        b.canonicalize().unwrap()
    );
    assert_eq!(
        found["project_key"].as_str().unwrap(),
        f.meta(&b, "project_key").unwrap(),
        "the registry must resolve to the project the store actually holds"
    );

    // The registry file is the one the environment named, and it is versioned.
    let raw = std::fs::read_to_string(f.registry()).unwrap();
    let doc: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(doc["registry_version"], 1);
}

/// The failure the whole slice exists to prevent: a name that is not there must
/// never quietly resolve to the store under the cwd.
#[test]
fn an_unknown_name_fails_loudly() {
    let f = Fixture::new();
    let a = f.init("alpha", &[]);
    f.ok(&a, &["project", "register"]);
    let err = f.fail(&a, &["project", "lookup", "not-a-project"]);
    assert!(err.contains("not-a-project"), "{err}");
}

#[test]
fn two_stores_may_not_claim_one_name() {
    let f = Fixture::new();
    let a = f.init("alpha", &["--name", "shared"]);
    let b = f.init("beta", &["--name", "shared"]);
    f.ok(&a, &["project", "register"]);
    let err = f.fail(&b, &["project", "register"]);
    assert!(
        err.contains(&a.canonicalize().unwrap().display().to_string()),
        "the registered store must be named: {err}"
    );
    assert!(
        err.contains(&b.canonicalize().unwrap().display().to_string()),
        "the store being refused must be named: {err}"
    );
    // And the refusal left the registry alone.
    assert_eq!(
        Path::new(
            f.json(&a, &["project", "lookup", "shared", "--json"])["path"]
                .as_str()
                .unwrap()
        ),
        a.canonicalize().unwrap()
    );
}

#[test]
fn a_registry_without_a_version_is_refused_rather_than_read() {
    let f = Fixture::new();
    let a = f.init("alpha", &[]);
    std::fs::create_dir_all(f.registry().parent().unwrap()).unwrap();
    std::fs::write(f.registry(), r#"{"projects":[]}"#).unwrap();
    let err = f.fail(&a, &["project", "list"]);
    assert!(err.contains("registry_version"), "{err}");
}

#[test]
fn unregister_is_the_remedy_for_a_stale_entry() {
    let f = Fixture::new();
    let a = f.init("alpha", &[]);
    f.ok(&a, &["project", "register"]);
    f.ok(&a, &["project", "unregister", "alpha"]);
    assert!(f.json(&a, &["project", "list", "--json"])["projects"]
        .as_array()
        .unwrap()
        .is_empty());
}

// ---------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------

#[test]
fn doctor_passes_on_a_named_and_registered_store() {
    let f = Fixture::new();
    let a = f.init("alpha", &[]);
    f.ok(&a, &["project", "register"]);
    for name in [
        "unique",
        "dangling",
        "name",
        "name_form",
        "registered",
        "entry_path",
    ] {
        assert_eq!(f.check(&a, name)["status"], "pass", "check `{name}`");
    }
    // And a clean store is not a fault, so the command still exits 0.
    f.qp(&a).args(["remote", "doctor"]).assert().success();
}

/// An unregistered store is told what to type, and is NOT called broken.
///
/// Every store starts unregistered and most stay that way, so failing the run
/// here would fire on the normal case — and a check that fires on the normal
/// case is one people learn to ignore. The note still carries the remedy, which
/// is the whole reason `Seq::advice` exists.
#[test]
fn doctor_advises_an_unregistered_store_without_failing_the_run() {
    let f = Fixture::new();
    let a = f.init("alpha", &[]);
    let c = f.check(&a, "registered");
    assert_eq!(c["status"], "note");
    assert!(
        c["remedy"]
            .as_str()
            .unwrap()
            .contains("qp project register"),
        "{c}"
    );
    f.qp(&a).args(["remote", "doctor"]).assert().success();
}

/// A genuine fault, by contrast, exits with the doctor's own fault code.
#[test]
fn doctor_exits_five_on_a_dangling_entry() {
    let f = Fixture::new();
    let a = f.init("alpha", &[]);
    let gone = f.init("ghost", &[]);
    f.ok(&gone, &["project", "register"]);
    std::fs::remove_dir_all(gone.parent().unwrap()).unwrap();
    f.qp(&a).args(["remote", "doctor"]).assert().code(5);
}

/// The unnamed shape cannot survive a command, which IS the guarantee.
///
/// `remote doctor` opens the store, and any open heals a missing name, so the
/// doctor's `name` failure is a guard for a caller that reads without healing
/// (a store on a read-only filesystem, or a server holding a connection it did
/// not open). What is observable here is the healing itself; the failure branch
/// is pinned by a unit test in `doctor` where the heal can be held back.
#[test]
fn a_deleted_name_is_healed_before_the_doctor_can_see_it_missing() {
    let f = Fixture::new();
    let a = f.init("alpha", &[]);
    {
        let conn = rusqlite::Connection::open(&a).unwrap();
        conn.execute("DELETE FROM meta WHERE key = 'project_name'", [])
            .unwrap();
    }
    assert_eq!(
        f.check(&a, "name")["status"],
        "note",
        "a healed name is derived"
    );
    assert!(f.meta(&a, "project_name").is_some());
}

#[test]
fn doctor_names_a_malformed_name() {
    let f = Fixture::new();
    let a = f.init("alpha", &[]);
    {
        let conn = rusqlite::Connection::open(&a).unwrap();
        conn.execute(
            "UPDATE meta SET value = 'Not A Name' WHERE key = 'project_name'",
            [],
        )
        .unwrap();
    }
    let c = f.check(&a, "name_form");
    assert_eq!(c["status"], "fail");
    assert_eq!(f.check(&a, "registered")["status"], "skipped");
}

#[test]
fn doctor_names_a_duplicate_in_the_registry() {
    let f = Fixture::new();
    let a = f.init("alpha", &[]);
    std::fs::create_dir_all(f.registry().parent().unwrap()).unwrap();
    std::fs::write(
        f.registry(),
        format!(
            r#"{{"registry_version":1,"projects":[
                 {{"name":"alpha","path":"{p}","project_key":"k","project_uuid":"u1"}},
                 {{"name":"alpha","path":"{p}","project_key":"k","project_uuid":"u2"}}]}}"#,
            p = a.display()
        ),
    )
    .unwrap();
    let c = f.check(&a, "unique");
    assert_eq!(c["status"], "fail");
    assert!(c["detail"].as_str().unwrap().contains("alpha"), "{c}");
    assert!(
        c["remedy"]
            .as_str()
            .unwrap()
            .contains("qp project unregister"),
        "{c}"
    );
}

#[test]
fn doctor_names_a_dangling_entry_as_its_own_fault() {
    let f = Fixture::new();
    let a = f.init("alpha", &[]);
    let gone = f.init("ghost", &[]);
    f.ok(&a, &["project", "register"]);
    f.ok(&gone, &["project", "register"]);
    std::fs::remove_dir_all(gone.parent().unwrap()).unwrap();

    let c = f.check(&a, "dangling");
    assert_eq!(c["status"], "fail");
    assert!(c["detail"].as_str().unwrap().contains("ghost"), "{c}");
    // And it is NOT reported as a duplicate: the two faults have two remedies.
    assert_eq!(f.check(&a, "unique")["status"], "pass");
}

#[test]
fn doctor_names_an_entry_that_points_at_a_different_store() {
    let f = Fixture::new();
    let a = f.init("alpha", &["--name", "alpha"]);
    let b = f.init("beta", &["--name", "alpha"]);
    f.ok(&b, &["project", "register"]);
    let c = f.check(&a, "entry_path");
    assert_eq!(c["status"], "fail");
    assert!(
        c["detail"].as_str().unwrap().contains("beta"),
        "the store the name really resolves to must be named: {c}"
    );
}

// ---------------------------------------------------------------------------
// the guard on this file
// ---------------------------------------------------------------------------

/// Nothing above may reach a real config directory.
///
/// Asserted rather than assumed: the override is one `env` call away from being
/// forgotten in a new test, and the damage — a developer's registry rewritten
/// by a test run — is invisible until it matters.
#[test]
fn the_real_config_directory_is_never_touched() {
    let f = Fixture::new();
    let a = f.init("alpha", &[]);
    f.ok(&a, &["project", "register"]);
    assert!(f.registry().exists(), "the override must be where we wrote");
    assert!(
        !f.root().join("xdg").join("quipu").exists(),
        "QUIPU_REGISTRY must win over the XDG lookup"
    );
    assert!(
        !f.root().join("home").join(".config").join("quipu").exists(),
        "nothing may fall back to a home directory"
    );
}

// ---------------------------------------------------------------------------
// the registry readers work where there is no store
// ---------------------------------------------------------------------------

/// Reading the registry must not require being attached to a project.
///
/// This is the seam between two changes that were written apart: one made every
/// command fail when there is no store, the other added commands that answer
/// from the registry file alone. Composed naively the second is unreachable
/// exactly where it is most needed — you reach for `qp project list` *because*
/// you do not know which store you are near, and a directory with no store is
/// the normal place to be standing when you do. Neither branch's own tests can
/// see this: it only exists once both are in the tree.
///
/// `register` is deliberately not in the list. It reads the store's name and
/// uuid, so it genuinely needs one, and must still refuse with `no_store`.
#[test]
fn registry_readers_do_not_need_a_store_but_register_does() {
    let f = Fixture::new();
    let nowhere = f.root().join("nowhere");
    std::fs::create_dir_all(&nowhere).unwrap();

    let bare = |args: &[&str]| {
        let mut c = Command::cargo_bin("qp").unwrap();
        c.current_dir(&nowhere);
        c.env("QUIPU_REGISTRY", f.registry());
        c.env("HOME", f.root().join("home"));
        c.env("XDG_CONFIG_HOME", f.root().join("xdg"));
        c.env_remove("QP_DB");
        c.env_remove("QUIPU_LEDGER");
        c.args(args);
        c
    };

    bare(&["project", "list", "--json"]).assert().success();
    // A name that is not registered is `not_found` (2), not `no_store` (6):
    // the question was answerable and the answer was no.
    bare(&["project", "lookup", "ghost", "--json"])
        .assert()
        .code(2);
    bare(&["project", "unregister", "ghost", "--json"])
        .assert()
        .code(2);
    bare(&["project", "register", "--json"]).assert().code(6);

    // And none of that may leave a store behind, which is the whole point of
    // the change on the other side of this seam.
    assert!(
        !nowhere.join(".quipu").exists(),
        "a registry read must not create a store"
    );
}
