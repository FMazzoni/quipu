//! Diagnosis for a configured remote: run the checks in order and NAME the cause.
//!
#![doc = include_str!("../docs/modules/doctor.md")]

use crate::remote::Remotes;
use crate::s3::{self, Fault, Scope};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The key a write probe puts and gets back. Fixed, flat, and outside the ledger's shape.
///
/// Fixed rather than unique because the probe cannot clean up after itself:
/// quipu's object client has no DELETE, deliberately — nothing in the ledger is
/// ever deleted, so requiring the permission would fail stores whose key is
/// correctly scoped without it. A fixed key is therefore overwritten by the next
/// run instead of accumulating. Flat, with no `/`, because a key containing one
/// would appear as a `CommonPrefixes` entry — that is, as a PROJECT — to
/// `Client::list_project_keys`.
const PROBE_KEY: &str = "_qp-doctor-probe";

/// Past this the object store will refuse the signature outright (SigV4 allows ~15 minutes).
const SKEW_FATAL: time::Duration = time::Duration::minutes(15);

/// Past this the clock is close enough to the limit to be worth saying so.
const SKEW_WARN: time::Duration = time::Duration::minutes(5);

/// One check's verdict.
///
/// `Note` exists so that a fact worth reporting is not forced to masquerade as
/// a pass or a failure: a store that is deliberately local-only is neither
/// broken nor fine, and a check that could not be run must not be counted as
/// one that succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[cfg_attr(test, default)]
    Pass,
    Fail,
    Note,
    Skipped,
}

impl Status {
    fn tag(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Note => "NOTE",
            Status::Skipped => "SKIP",
        }
    }
}

/// One check: what was looked at, what happened, and what to do about it.
///
/// `remedy` is the reason this command exists. A diagnosis nobody can act on is
/// a slower error message, so every `Fail` carries one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    /// Which sequence this belongs to: `config`, `registry`, `project` or `ledger`.
    pub target: String,
    /// Stable machine name for the check, for a script that branches on one.
    pub name: String,
    pub status: Status,
    /// What was checked and what came back.
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

/// The whole diagnosis.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub struct Report {
    pub ledger: Option<String>,
    pub local_only: bool,
    /// False when any check failed — the same condition as the non-zero exit.
    pub ok: bool,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn failed(&self) -> bool {
        !self.ok
    }
}

impl crate::outcome::Outcome for Report {
    fn human(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        let mut current = "";
        for c in &self.checks {
            if c.target != current {
                current = c.target.as_str();
                let url = match c.target.as_str() {
                    "ledger" => self.ledger.as_deref(),
                    _ => None,
                };
                out.push(match url {
                    Some(u) => format!("{}  {u}", c.target),
                    None => c.target.to_string(),
                });
            }
            out.push(format!(
                "  {}  {:<12}  {}",
                c.status.tag(),
                c.name,
                c.detail
            ));
            if let Some(r) = &c.remedy {
                for (i, line) in r.lines().enumerate() {
                    out.push(format!("        {} {line}", if i == 0 { "→" } else { " " }));
                }
            }
        }
        out.push(String::new());
        out.push(if self.ok {
            "no faults found".to_string()
        } else {
            "FAULTS FOUND — see the arrows above".to_string()
        });
        out.join("\n")
    }
}

/// Accumulates one remote's checks and enforces "stop at the first failure".
///
/// The rule is not a nicety. A missing credential makes every later check
/// answer a question nobody asked, and a screen of red herrings is exactly the
/// experience this command exists to replace — so once something fails, the
/// rest of that remote's sequence is recorded as skipped and never run.
struct Seq<'a> {
    target: &'static str,
    checks: &'a mut Vec<Check>,
}

impl<'a> Seq<'a> {
    fn new(target: &'static str, checks: &'a mut Vec<Check>) -> Self {
        Self { target, checks }
    }

    fn push(&mut self, name: &'static str, status: Status, detail: String, remedy: Option<String>) {
        self.checks.push(Check {
            target: self.target.to_string(),
            name: name.to_string(),
            status,
            detail,
            remedy,
        });
    }

    fn pass(&mut self, name: &'static str, detail: impl Into<String>) {
        self.push(name, Status::Pass, detail.into(), None);
    }

    fn note(&mut self, name: &'static str, detail: impl Into<String>) {
        self.push(name, Status::Note, detail.into(), None);
    }

    /// Records something to do about a state that is not a fault.
    ///
    /// A `Note` normally carries no remedy, because there is nothing to fix.
    /// This is the case in between: not being registered is neither broken nor
    /// finished — the store works, and nothing can reach it by name — so it
    /// must not fail the run, and it must still say what to type.
    fn advice(&mut self, name: &'static str, detail: impl Into<String>, remedy: impl Into<String>) {
        self.push(name, Status::Note, detail.into(), Some(remedy.into()));
    }

    /// Records a failure and the remedy that must accompany one.
    ///
    /// Stopping the sequence is the CALLER's job — every caller returns right
    /// after — because only the caller knows which later checks are now
    /// meaningless and must be marked skipped rather than silently dropped.
    fn fail(&mut self, name: &'static str, detail: impl Into<String>, remedy: impl Into<String>) {
        self.push(name, Status::Fail, detail.into(), Some(remedy.into()));
    }

    /// Marks a check that was never run because an earlier one failed.
    fn skip(&mut self, name: &'static str) {
        self.push(
            name,
            Status::Skipped,
            "not run: an earlier check failed, so this one could not mean anything".to_string(),
            None,
        );
    }
}

/// Runs every check for whatever is configured. The only entry point.
pub fn run(db_path: &Path, r: &Remotes) -> Report {
    let mut checks: Vec<Check> = Vec::new();

    {
        let mut s = Seq::new("config", &mut checks);
        match r.ledger.is_some() {
            false => s.note(
                "configured",
                "local-only: no ledger. Nothing to check, and nothing wrong — attach one \
                 with `qp remote set --ledger`.",
            ),
            true => s.pass(
                "configured",
                "ledger set: the event log syncs, claims are arbitrated locally",
            ),
        }
    }

    // Unconditional, unlike everything below: naming and the registry are what
    // a caller uses to reach this store at all, and they are wrong in exactly
    // the same way whether or not a remote is attached.
    let reg = crate::registry::load();
    registry_checks(&reg, &mut Seq::new("registry", &mut checks));
    project_checks(db_path, &reg, &mut Seq::new("project", &mut checks));

    if let Some(url) = &r.ledger {
        ledger_checks(url, &mut Seq::new("ledger", &mut checks));
    }

    let ok = !checks.iter().any(|c| c.status == Status::Fail);
    Report {
        ledger: r.ledger.clone(),
        local_only: r.local_only(),
        ok,
        checks,
    }
}

// ---------------------------------------------------------------------------
// naming and the registry
// ---------------------------------------------------------------------------

/// The registry file itself: readable, versioned, and internally consistent.
///
/// Separate from the `project` sequence below because the faults are separate.
/// A duplicate somewhere else in the file says nothing about whether THIS store
/// is named correctly, and folding them into one stop-at-first-failure sequence
/// would hide the one behind the other.
fn registry_checks(reg: &anyhow::Result<crate::registry::Registry>, s: &mut Seq) {
    let reg = match reg {
        Ok(r) => r,
        Err(e) => {
            s.fail(
                "file",
                format!("{e:#}"),
                format!(
                    "The registry is the only map from a project name to a store, so quipu \
                     refuses to read one it does not understand rather than resolve a name \
                     to the wrong file. Fix the file, or move it aside and re-run \
                     `qp project register` in each store. `{}` points somewhere else entirely.",
                    crate::registry::ENV_REGISTRY
                ),
            );
            s.skip("unique");
            s.skip("dangling");
            return;
        }
    };
    let dups = reg.duplicates();
    if dups.is_empty() {
        s.pass(
            "unique",
            format!(
                "{} project(s) registered in {}, no name claimed twice",
                reg.entries.len(),
                reg.path.display()
            ),
        );
    } else {
        let listed: Vec<String> = dups
            .iter()
            .map(|(name, paths)| {
                let ps: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                format!("{name}: {}", ps.join(", "))
            })
            .collect();
        s.fail(
            "unique",
            format!(
                "a name is claimed by more than one store — {}",
                listed.join("; ")
            ),
            "A lookup on that name resolves to whichever entry comes first, which is not a \
             decision anybody made. Drop the wrong one with `qp project unregister <NAME>`, \
             then give the store that lost its entry a distinct name at \
             `qp init --name <NAME>` and re-register it.",
        );
        s.skip("dangling");
        return;
    }
    let dangling = reg.dangling();
    if dangling.is_empty() {
        s.pass(
            "dangling",
            "every registered store is still where the registry says",
        );
    } else {
        let listed: Vec<String> = dangling
            .iter()
            .map(|e| format!("{} -> {}", e.name, e.path.display()))
            .collect();
        s.fail(
            "dangling",
            format!(
                "registry entries point at stores that are gone — {}",
                listed.join("; ")
            ),
            "The store moved or was deleted, and an index cannot notice on its own. Re-run \
             `qp project register` from the store's new location, or drop the entry with \
             `qp project unregister <NAME>`.",
        );
    }
}

/// This store's own name, and whether the registry agrees about it.
fn project_checks(db_path: &Path, reg: &anyhow::Result<crate::registry::Registry>, s: &mut Seq) {
    let conn = match rusqlite::Connection::open(db_path) {
        Ok(c) => c,
        Err(e) => {
            s.fail(
                "name",
                format!("cannot open the store at {}: {e}", db_path.display()),
                "Run `qp init` here, or point `--db` / `QP_DB` at the store you meant.",
            );
            s.skip("name_form");
            s.skip("registered");
            s.skip("entry_path");
            return;
        }
    };
    let name = match crate::project::read_name(&conn) {
        Ok(Some(n)) => n,
        _ => {
            s.fail(
                "name",
                format!("the store at {} has no project_name", db_path.display()),
                "Every store carries the name it answers to, so the name survives a move and \
                 cannot drift from the data it names. Run any qp command to have one derived, \
                 or `qp init --name <NAME>` to choose it.",
            );
            s.skip("name_form");
            s.skip("registered");
            s.skip("entry_path");
            return;
        }
    };
    let derived = crate::project::name_origin(&conn).ok().flatten().as_deref()
        == Some(crate::project::ORIGIN_DERIVED);
    if derived {
        s.note(
            "name",
            format!(
                "`{name}` was derived when this store was migrated, not chosen. It works; \
                 `qp init --name <NAME>` replaces it with something a person would type."
            ),
        );
    } else {
        s.pass("name", format!("named `{name}`"));
    }

    if let Err(e) = crate::project::validate_name(&name) {
        s.fail(
            "name_form",
            format!("{e:#}"),
            "A name is a shell argument and a lookup key, so a spelling that quotes or \
             normalises two ways is two projects. Set a well-formed one with \
             `qp init --name <NAME>`.",
        );
        s.skip("registered");
        s.skip("entry_path");
        return;
    }
    s.pass(
        "name_form",
        format!("`{name}` is a well-formed project name"),
    );

    let Ok(reg) = reg else {
        s.skip("registered");
        s.skip("entry_path");
        return;
    };
    let Some(entry) = reg.lookup(&name) else {
        // Deliberately not a fault. Every store starts unregistered, and one
        // that never is still works for everything that finds its store by
        // walking up from the cwd — which today is every command. Failing the
        // run here would make `qp remote doctor` exit non-zero on a perfectly
        // healthy store, and a check that fires on the normal case is one
        // people learn to ignore.
        s.advice(
            "registered",
            format!("`{name}` is not in {}", reg.path.display()),
            "Nothing can reach this store by NAME until it is indexed — registration is an \
             explicit act, never a side effect. Run `qp project register` here.",
        );
        s.skip("entry_path");
        return;
    };
    s.pass("registered", format!("`{name}` is registered"));

    let same = match (entry.path.canonicalize(), db_path.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        // An entry pointing at a file that is gone is the registry's own
        // `dangling` fault, already reported; here it simply is not this store.
        _ => entry.path == db_path,
    };
    if same {
        s.pass(
            "entry_path",
            format!("the entry for `{name}` points back at this store"),
        );
    } else {
        s.fail(
            "entry_path",
            format!(
                "`{name}` resolves to {}, which is not this store ({})",
                entry.path.display(),
                db_path.display()
            ),
            "Two stores are disagreeing about one name, so a lookup reaches the other one's \
             tickets. Give this store a distinct name with `qp init --name <NAME>` and \
             re-register, or `qp project unregister` the stale entry.",
        );
    }
}

// ---------------------------------------------------------------------------
// object storage
// ---------------------------------------------------------------------------

/// The ledger sequence, in the order a failure makes the rest meaningless.
///
/// The order is the diagnosis. Clock comes BEFORE the credential verdict on
/// purpose: a clock a quarter of an hour out makes the store reject a perfectly
/// good key, and reporting that as "the credentials were rejected" is the exact
/// error that sends people to rotate keys that were never wrong.
fn ledger_checks(url: &str, s: &mut Seq) {
    let loc = match s3::parse(url) {
        Ok(l) => l,
        Err(e) => {
            s.fail(
                "url",
                e.to_string(),
                "the ledger url is `s3://<host>/<bucket>[/<prefix>]`. Re-set it with \
                 `qp remote set --ledger <url>`.",
            );
            return skip_rest_of_ledger(s, "url");
        }
    };
    s.pass(
        "url",
        format!(
            "bucket `{}` at {}{}",
            loc.bucket,
            loc.host,
            if loc.prefix.is_empty() {
                String::new()
            } else {
                format!(", under prefix `{}/`", loc.prefix)
            }
        ),
    );

    let client = match s3::Client::open(url) {
        Ok(c) => c,
        Err(e @ Fault::NoCredentials) => {
            s.fail(
                "credentials",
                e.to_string(),
                format!(
                    "export {} and {} for the key that owns this bucket (plus {} if they are \
                     temporary). This is absence, not rejection: a wrong key fails differently.",
                    s3::ENV_KEY,
                    s3::ENV_SECRET,
                    s3::ENV_TOKEN
                ),
            );
            return skip_rest_of_ledger(s, "credentials");
        }
        Err(e) => {
            s.fail(
                "url",
                e.to_string(),
                "re-set the ledger url with `qp remote set --ledger <url>`.",
            );
            return skip_rest_of_ledger(s, "url");
        }
    };
    s.pass(
        "credentials",
        format!(
            "{} and {} are set in the environment",
            s3::ENV_KEY,
            s3::ENV_SECRET
        ),
    );

    let answer = client.probe(Scope::Prefix);

    // Endpoint and trust: a transport failure never got as far as a signature,
    // so nothing below it can be believed.
    match &answer.result {
        Err(Fault::Unreachable { endpoint, detail }) => {
            s.fail(
                "endpoint",
                format!("no answer from {endpoint}: {detail}"),
                unreachable_remedy(endpoint),
            );
            return skip_rest_of_ledger(s, "endpoint");
        }
        Err(Fault::Tls { endpoint, detail }) => {
            s.pass("endpoint", format!("{endpoint} answered"));
            s.fail(
                "tls",
                format!("the certificate was not trusted: {detail}"),
                "the endpoint is up, so this is trust rather than reachability. Install the \
                 CA that signed the endpoint's certificate on this machine, or refresh a \
                 stale cert store. A certificate issued by a private or mesh-VPN CA is not \
                 trusted by a machine outside that network.",
            );
            return skip_rest_of_ledger(s, "tls");
        }
        _ => {}
    }
    s.pass("endpoint", format!("{} answered", client.endpoint()));
    if client.endpoint().starts_with("https://") {
        s.pass("tls", "the certificate was trusted");
    } else {
        s.note(
            "tls",
            format!(
                "plaintext http, permitted only because the endpoint is loopback and {} is set",
                s3::ENV_PLAINTEXT
            ),
        );
    }

    // The clock, from the Date on that same response — which arrives whether the
    // request was served or refused.
    match clock_verdict(answer.server_date.as_deref(), &answer.result) {
        Clock::Fatal { detail } => {
            s.fail("clock", detail, SKEW_REMEDY);
            return skip_rest_of_ledger(s, "clock");
        }
        Clock::Near { detail } => s.note("clock", detail),
        Clock::Fine { detail } => s.pass("clock", detail),
        Clock::Unknown => s.note(
            "clock",
            "the store sent no usable Date header, so skew was not checked. A clock more \
             than ~15 minutes off makes SigV4 refuse every request.",
        ),
    }

    // Only now is a credential verdict meaningful.
    if let Err(Fault::CredentialsRejected { code }) = &answer.result {
        s.fail(
            "signature",
            format!("the store rejected the credentials ({code})"),
            format!(
                "{} and {} are set, and the clock is fine, so the key or the secret is wrong \
                 — not missing. Check the pair against the store, and check {} if these are \
                 temporary credentials.",
                s3::ENV_KEY,
                s3::ENV_SECRET,
                s3::ENV_TOKEN
            ),
        );
        return skip_rest_of_ledger(s, "signature");
    }
    s.pass("signature", "the store accepted the signed request");

    // Bucket versus policy. These two arrive on the same call with opposite
    // fixes, so they get a second call rather than a guess.
    let bucket = client.bucket_name().to_string();
    match &answer.result {
        Ok(()) => {
            s.pass("bucket", format!("`{bucket}` exists"));
            s.pass(
                "list",
                match client.prefix() {
                    "" => format!("the key can list `{bucket}`"),
                    p => format!("the key can list `{p}` in `{bucket}`"),
                },
            );
        }
        Err(Fault::BucketMissing { .. }) => {
            s.fail(
                "bucket",
                format!("the store has no bucket named `{bucket}`"),
                format!(
                    "create `{bucket}` on the store, or point the ledger at the bucket that \
                     exists. This is not a permission problem: the store answered by name."
                ),
            );
            return skip_rest_of_ledger(s, "bucket");
        }
        Err(Fault::Denied { .. }) => {
            // Denied on our own prefix. The bucket may not exist, or the key may
            // not be scoped to it — a second LIST at the root is what tells the
            // two apart, and it cannot always.
            match denied_verdict(&client, &bucket) {
                Denied::BucketMissing => {
                    s.fail(
                        "bucket",
                        format!(
                            "the store has no bucket named `{bucket}` — listing its root came \
                             back NoSuchBucket"
                        ),
                        format!(
                            "create `{bucket}`, or point the ledger at the bucket that exists."
                        ),
                    );
                    return skip_rest_of_ledger(s, "bucket");
                }
                Denied::NotScoped => {
                    s.pass(
                        "bucket",
                        format!("`{bucket}` exists — its root listed successfully"),
                    );
                    s.fail(
                        "list",
                        format!("the key may not list `{}` in `{bucket}`", client.prefix()),
                        format!(
                            "the bucket is there and the key is good, so this is the POLICY: \
                             grant s3:ListBucket on `{bucket}` for the prefix `{}`. Creating \
                             the bucket would not help — it already exists.",
                            client.prefix()
                        ),
                    );
                    return skip_rest_of_ledger(s, "list");
                }
                Denied::Ambiguous => {
                    s.note(
                        "bucket",
                        format!(
                            "undetermined: listing the root of `{bucket}` was also denied, so \
                             a missing bucket and an unscoped key look identical from here"
                        ),
                    );
                    s.fail(
                        "list",
                        format!("access to `{bucket}` was denied, and the cause cannot be narrowed further"),
                        format!(
                            "TWO possibilities, and this call cannot separate them. Either the \
                             bucket `{bucket}` does not exist, or the key is not scoped to it \
                             (an S3 store answers AccessDenied for a bucket you may not see, \
                             whether or not it is there). Check the bucket exists from an \
                             admin credential first; if it does, the fix is the key's policy, \
                             not a new bucket."
                        ),
                    );
                    return skip_rest_of_ledger(s, "list");
                }
            }
        }
        Err(e) => {
            s.fail(
                "list",
                e.to_string(),
                "the store answered, and this is its own diagnosis. Fix what it names.",
            );
            return skip_rest_of_ledger(s, "list");
        }
    }

    // PUT and GET are separate because a policy can allow one and not the other,
    // and sync needs both: it writes its own batches and reads everyone else's.
    let body = probe_body();
    if let Err(e) = client.put(PROBE_KEY, body.as_bytes()) {
        s.fail(
            "put",
            format!("the key cannot write in this prefix: {e}"),
            format!(
                "listing worked, so the bucket and the credentials are fine and this is the \
                 policy: grant s3:PutObject under `{}` in `{bucket}`. Without it `qp sync` \
                 can read the ledger and never add to it.",
                client.prefix()
            ),
        );
        return skip_rest_of_ledger(s, "put");
    }
    s.pass(
        "put",
        format!("wrote `{}{PROBE_KEY}` (overwritten by each run, never deleted — quipu's key needs no delete permission)", client.prefix()),
    );

    match client.get(PROBE_KEY) {
        Ok(got) if got == body.as_bytes() => s.pass("get", "read the probe object back intact"),
        Ok(_) => s.fail(
            "get",
            "the probe object read back with different bytes than were written",
            "something between here and the store is rewriting object bodies. Do not sync \
             to this endpoint until that is understood.",
        ),
        Err(e) => s.fail(
            "get",
            format!("the object just written could not be read back: {e}"),
            format!(
                "the write succeeded, so this is a read permission rather than a missing \
                 object: grant s3:GetObject under `{}` in `{bucket}`. A store that can write \
                 but not read cannot merge anyone else's history.",
                client.prefix()
            ),
        ),
    }
}

/// Names every ledger check that will not run, once one has failed.
fn skip_rest_of_ledger(s: &mut Seq, after: &str) {
    let all = [
        "url",
        "credentials",
        "endpoint",
        "tls",
        "clock",
        "signature",
        "bucket",
        "list",
        "put",
        "get",
    ];
    let from = all.iter().position(|n| *n == after).map_or(0, |i| i + 1);
    for name in &all[from..] {
        s.skip(name);
    }
}

fn probe_body() -> String {
    format!(
        "{{\"probe\":\"qp remote doctor\",\"at\":\"{}\"}}",
        crate::time::now_rfc3339()
    )
}

/// The three answers a second, root-level LIST can give after a denial.
enum Denied {
    BucketMissing,
    NotScoped,
    Ambiguous,
}

fn denied_verdict(client: &s3::Client, _bucket: &str) -> Denied {
    // With no prefix in the url the second call is the same call, so there is
    // nothing left to learn: say so rather than repeat it.
    if client.prefix().is_empty() {
        return Denied::Ambiguous;
    }
    match client.probe(Scope::Root).result {
        Ok(()) => Denied::NotScoped,
        Err(Fault::BucketMissing { .. }) => Denied::BucketMissing,
        _ => Denied::Ambiguous,
    }
}

const SKEW_REMEDY: &str = "fix THIS MACHINE'S CLOCK (enable NTP), or the store's. Do not rotate \
     the credentials: SigV4 signs the timestamp, so a clock more than ~15 minutes out makes \
     the store refuse a key that is perfectly good, and the error it returns says nothing \
     about time.";

enum Clock {
    Fatal { detail: String },
    Near { detail: String },
    Fine { detail: String },
    Unknown,
}

/// Reads the clock two ways: what the store said outright, and what its `Date` header shows.
///
/// The explicit fault comes first because it is the store's own verdict. The
/// header is what catches a clock that is already far out while the store is
/// still, for now, accepting requests.
fn clock_verdict(date: Option<&str>, result: &Result<(), Fault>) -> Clock {
    if let Err(Fault::ClockSkew { code }) = result {
        let observed = date
            .and_then(crate::time::parse_http_date)
            .map(|d| format!("; the store's clock reads {d}"))
            .unwrap_or_default();
        return Clock::Fatal {
            detail: format!(
                "the store refused the request's timestamp ({code}){observed}. This is the \
                 clock, not the key."
            ),
        };
    }
    let Some(server) = date.and_then(crate::time::parse_http_date) else {
        return Clock::Unknown;
    };
    let delta = server - crate::time::now_utc();
    let mag = delta.abs();
    // `delta` is the store's clock minus this one's, so a POSITIVE delta means
    // the store is ahead and therefore that this machine is behind. Getting
    // this backwards sends someone to fix the wrong clock.
    let side = if delta.is_negative() {
        "ahead of"
    } else {
        "behind"
    };
    let off = format!(
        "this machine's clock is {} {side} the store's",
        human_duration(mag)
    );
    if mag >= SKEW_FATAL {
        Clock::Fatal {
            detail: format!("{off} — past the ~15 minutes SigV4 allows"),
        }
    } else if mag >= SKEW_WARN {
        Clock::Near {
            detail: format!("{off}, which is close to the ~15 minutes SigV4 allows"),
        }
    } else {
        Clock::Fine {
            detail: format!("within {} of the store's", human_duration(SKEW_WARN)),
        }
    }
}

fn human_duration(d: time::Duration) -> String {
    let s = d.whole_seconds();
    if s >= 120 {
        format!("{} minutes", s / 60)
    } else {
        format!("{s} seconds")
    }
}

fn unreachable_remedy(endpoint: &str) -> String {
    format!(
        "nothing answered at {endpoint}. THREE causes look identical here and this probe \
         cannot separate them:\n\
         (1) the service is down — check the container or daemon on that host;\n\
         (2) the address is stale — a host that moved onto a private mesh network no longer \
         answers on its old LAN address, and the refusal is byte-identical;\n\
         (3) this machine is not on that network, or its node is quarantined by the mesh's \
         own access control — a node can register, obtain a certificate and still be locked \
         out, in which case it is absent from the network's status listing.\n\
         Resolve the host by name from this machine before touching the service."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry with nothing in it, at a path nothing will be written to.
    fn empty_registry(dir: &std::path::Path) -> anyhow::Result<crate::registry::Registry> {
        Ok(crate::registry::Registry {
            path: dir.join("registry.json"),
            entries: Vec::new(),
        })
    }

    /// The `name` failure branch, which no CLI path can reach.
    ///
    /// Every open heals a missing name, so the branch is held back here by
    /// giving the store a `meta` table with no `project_uuid` — the one shape
    /// the backfill declines to name.
    #[test]
    fn a_store_with_no_name_is_a_named_fault_and_stops_the_sequence() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();
        let reg = empty_registry(tmp.path());
        let mut checks = Vec::new();
        project_checks(&db, &reg, &mut Seq::new("project", &mut checks));
        assert_eq!(checks[0].name, "name");
        assert_eq!(checks[0].status, Status::Fail);
        assert!(checks[0].remedy.as_ref().unwrap().contains("--name"));
        assert!(
            checks[1..].iter().all(|c| c.status == Status::Skipped),
            "a store with no name makes every later check meaningless"
        );
    }

    /// An unreadable registry must not be reported as an empty one.
    #[test]
    fn an_unreadable_registry_fails_and_skips_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("registry.json");
        std::fs::write(&p, "{not json").unwrap();
        let reg = crate::registry::load_from(&p);
        let mut checks = Vec::new();
        registry_checks(&reg, &mut Seq::new("registry", &mut checks));
        assert_eq!(checks[0].name, "file");
        assert_eq!(checks[0].status, Status::Fail);
        assert!(checks[1..].iter().all(|c| c.status == Status::Skipped));
    }

    #[test]
    fn a_skewed_clock_is_reported_as_the_clock_even_when_the_store_blamed_the_key() {
        // What a store actually returns for a clock 20 minutes out.
        let v = clock_verdict(
            None,
            &Err(Fault::ClockSkew {
                code: "RequestTimeTooSkewed".to_string(),
            }),
        );
        let Clock::Fatal { detail } = v else {
            panic!("a declared skew must be fatal");
        };
        assert!(detail.contains("clock, not the key"), "{detail}");
    }

    fn imf(t: time::OffsetDateTime) -> String {
        t.format(&time::format_description::well_known::Rfc2822)
            .unwrap()
            .replace("+0000", "GMT")
    }

    /// Also pins which way round the sentence reads.
    ///
    /// The direction is not cosmetic: it is the difference between telling
    /// someone to fix this machine's clock and telling them to fix the store's.
    /// It was backwards when first written, and only this assertion said so.
    #[test]
    fn the_date_header_catches_a_skew_the_store_has_not_yet_refused() {
        let store_ahead = imf(crate::time::now_utc() + time::Duration::minutes(40));
        let Clock::Fatal { detail } = clock_verdict(Some(&store_ahead), &Ok(())) else {
            panic!("40 minutes is past what SigV4 allows");
        };
        assert!(
            detail.contains("behind") && !detail.contains("ahead"),
            "the store's clock is ahead, so this machine's is behind: {detail}"
        );

        let store_behind = imf(crate::time::now_utc() - time::Duration::minutes(40));
        let Clock::Fatal { detail } = clock_verdict(Some(&store_behind), &Ok(())) else {
            panic!("40 minutes is past what SigV4 allows");
        };
        assert!(detail.contains("ahead of"), "{detail}");
    }

    #[test]
    fn a_clock_within_tolerance_passes_and_a_missing_header_is_not_a_skew() {
        let now = crate::time::now_utc()
            .format(&time::format_description::well_known::Rfc2822)
            .unwrap()
            .replace("+0000", "GMT");
        assert!(matches!(
            clock_verdict(Some(&now), &Ok(())),
            Clock::Fine { .. }
        ));
        assert!(matches!(clock_verdict(None, &Ok(())), Clock::Unknown));
    }

    /// A report with checks in it, which the empty `Default` sample cannot reach.
    ///
    /// `remedy` carries `skip_serializing_if` and `Status` is an enum whose wire
    /// form is its own `rename_all` spelling.
    #[test]
    fn a_populated_report_round_trips() {
        crate::outcome::round_trip::assert_round_trips(&Report {
            ledger: Some("s3://example/ledger".to_string()),
            local_only: false,
            ok: false,
            checks: vec![
                Check {
                    target: "config".to_string(),
                    name: "prefix".to_string(),
                    status: Status::Pass,
                    detail: "prefix is set".to_string(),
                    remedy: None,
                },
                Check {
                    target: "authority".to_string(),
                    name: "reachable".to_string(),
                    status: Status::Fail,
                    detail: "could not connect".to_string(),
                    remedy: Some("check the url".to_string()),
                },
            ],
        });
    }
}
