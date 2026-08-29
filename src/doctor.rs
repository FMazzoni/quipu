//! Diagnosis for a configured remote: run the checks in order and NAME the cause.
//!
#![doc = include_str!("../docs/modules/doctor.md")]

use crate::claimref::{Arbiter, WriteProbe};
use crate::remote::Remotes;
use crate::s3::{self, Fault, Scope};
use serde::Serialize;
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

/// Ref the authority write probe creates and removes. Obviously temporary, obviously ours.
const PROBE_REF_PREFIX: &str = "refs/quipu/doctor/probe-";

/// Past this the object store will refuse the signature outright (SigV4 allows ~15 minutes).
const SKEW_FATAL: time::Duration = time::Duration::minutes(15);

/// Past this the clock is close enough to the limit to be worth saying so.
const SKEW_WARN: time::Duration = time::Duration::minutes(5);

/// One check's verdict.
///
/// `Note` exists so that a fact worth reporting is not forced to masquerade as
/// a pass or a failure: multiplexing being off is neither broken nor fine, and
/// a check that could not be run must not be counted as one that succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
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
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// Which remote this belongs to: `config`, `ledger` or `authority`.
    pub target: &'static str,
    /// Stable machine name for the check, for a script that branches on one.
    pub name: &'static str,
    pub status: Status,
    /// What was checked and what came back.
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

/// The whole diagnosis.
#[derive(Debug, Serialize)]
pub struct Report {
    pub authority: Option<String>,
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
                current = c.target;
                let url = match c.target {
                    "ledger" => self.ledger.as_deref(),
                    "authority" => self.authority.as_deref(),
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
            target: self.target,
            name,
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
        match (r.authority.is_some(), r.ledger.is_some()) {
            (false, false) => s.note(
                "configured",
                "local-only: no authority and no ledger. Nothing to check, and nothing wrong \
                 — attach one with `qp remote set`.",
            ),
            (true, false) => s.pass(
                "configured",
                "authority only: claims are refereed, the event log stays on this machine",
            ),
            (false, true) => s.pass(
                "configured",
                "ledger only: the event log syncs, claims are arbitrated locally",
            ),
            (true, true) => s.pass("configured", "authority and ledger both set"),
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
    if let Some(url) = &r.authority {
        authority_checks(db_path, url, &mut Seq::new("authority", &mut checks));
    }

    let ok = !checks.iter().any(|c| c.status == Status::Fail);
    Report {
        authority: r.authority.clone(),
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
            format!("a name is claimed by more than one store — {}", listed.join("; ")),
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
        s.pass("dangling", "every registered store is still where the registry says");
    } else {
        let listed: Vec<String> = dangling
            .iter()
            .map(|e| format!("{} -> {}", e.name, e.path.display()))
            .collect();
        s.fail(
            "dangling",
            format!("registry entries point at stores that are gone — {}", listed.join("; ")),
            "The store moved or was deleted, and an index cannot notice on its own. Re-run \
             `qp project register` from the store's new location, or drop the entry with \
             `qp project unregister <NAME>`.",
        );
    }
}

/// This store's own name, and whether the registry agrees about it.
fn project_checks(
    db_path: &Path,
    reg: &anyhow::Result<crate::registry::Registry>,
    s: &mut Seq,
) {
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
    s.pass("name_form", format!("`{name}` is a well-formed project name"));

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

// ---------------------------------------------------------------------------
// the claim authority
// ---------------------------------------------------------------------------

/// The authority sequence: git present, the remote answers, the namespace is WRITABLE.
///
/// The last one is the point. `git ls-remote` succeeds against a repository
/// nobody may write, and a claim is a ref creation — so a read probe passing
/// proves nothing about whether a single claim will ever work.
fn authority_checks(db_path: &Path, url: &str, s: &mut Seq) {
    match std::process::Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(st) if st.success() => s.pass("git", "git is on this machine's PATH"),
        _ => {
            s.fail(
                "git",
                "git could not be run on THIS machine",
                "install git, or put it on the PATH of whatever runs qp. The authority is a \
                 git remote and quipu shells out to git for every claim.",
            );
            s.skip("reach");
            s.skip("write");
            s.skip("multiplex");
            return;
        }
    }

    let out = crate::remote::git(db_path, url, "remote-doctor")
        .args(["ls-remote", "--", url])
        .output();
    match out {
        Err(e) => {
            s.fail(
                "reach",
                format!("could not spawn git: {e}"),
                "install git, or put it on the PATH of whatever runs qp.",
            );
            s.skip("write");
            s.skip("multiplex");
            return;
        }
        Ok(o) if !o.status.success() => {
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            let (detail, remedy) = diagnose_git(url, &stderr);
            s.fail("reach", detail, remedy);
            s.skip("write");
            multiplex_check(db_path, url, s);
            return;
        }
        Ok(_) => s.pass("reach", "the authority answered `git ls-remote`"),
    }

    let refname = format!("{PROBE_REF_PREFIX}{}", uuid::Uuid::new_v4());
    let arb = Arbiter::new(db_path, url.to_string(), "remote-doctor");
    match arb.probe_namespace(&refname) {
        WriteProbe::Ok => s.pass(
            "write",
            "created and removed a probe ref under refs/quipu/ — a claim can be taken and released",
        ),
        WriteProbe::CreateFailed(why) => s.fail(
            "write",
            format!("the authority refused a ref under refs/quipu/: {why}"),
            "reading worked and writing did not, which is what a claim needs. Either the \
             account has read-only access to this repository, or the repository is served \
             read-only. A claim is a ref CREATION; nothing about this store will work until \
             a push under refs/quipu/ is accepted.",
        ),
        WriteProbe::Leftover { refname } => s.fail(
            "write",
            format!("the probe ref was created but could not be removed; `{refname}` is still on the authority"),
            format!(
                "delete it by hand — `git push {url} :{refname}` — then work out why the \
                 delete was refused. A claim that cannot be released is a ticket nobody can \
                 claim again."
            ),
        ),
    }
    multiplex_check(db_path, url, s);
}

/// Whether claims over this url will share one ssh connection, and what it costs when they do not.
///
/// Informational, never a failure: a claim without multiplexing works, it just
/// pays a full ssh handshake every time. The number is worth saying out loud
/// because it is invisible — a claim taking most of a second looks like quipu
/// being slow rather than like a connection being rebuilt.
fn multiplex_check(db_path: &Path, url: &str, s: &mut Seq) {
    if !crate::remote::is_ssh_url(url) {
        s.note(
            "multiplex",
            "not an ssh url, so no connection is built per claim and there is nothing to share",
        );
        return;
    }
    match crate::remote::ssh_command(db_path, url, "remote-doctor") {
        Some(_) => s.pass(
            "multiplex",
            "ssh connections are shared per agent, so a claim after the first reuses an \
             open connection instead of building one",
        ),
        None => s.note(
            "multiplex",
            "connection sharing is OFF for this ssh url — GIT_SSH_COMMAND, GIT_SSH or \
             QUIPU_SSH_MULTIPLEX in the environment turns quipu's injection off, and a \
             control path longer than the socket limit does too. Every claim then pays a \
             full ssh handshake: hundreds of milliseconds each, against tens with sharing.",
        ),
    }
}

/// Which transport a url reaches the authority over. Not cosmetic — it changes the diagnosis.
///
/// The same sentence from git means different things depending on how it got
/// there, and the difference is the whole product of this command. `git://` has
/// no authentication whatsoever, so an "access denied" from a git daemon can
/// only be about the repository; reading it as a credential problem sends
/// someone to inspect an ssh key that is not involved in the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Ssh,
    /// The git protocol: anonymous by construction, no user, no credential.
    GitDaemon,
    Http,
    /// A filesystem path. No transport, no daemon, and no authentication either.
    Local,
}

impl Transport {
    fn of(url: &str) -> Self {
        if crate::remote::is_ssh_url(url) {
            Transport::Ssh
        } else if url.starts_with("git://") {
            Transport::GitDaemon
        } else if url.starts_with("http://") || url.starts_with("https://") {
            Transport::Http
        } else {
            Transport::Local
        }
    }

    /// Whether a credential can be wrong on this transport at all.
    ///
    /// False for `git://` and for a local path, and that is not a nicety: git
    /// says "access denied" over the git protocol when a repository is absent,
    /// unexported or owned by the wrong user, and there is no credential in the
    /// request for any of those to be about.
    fn authenticates(self) -> bool {
        matches!(self, Transport::Ssh | Transport::Http)
    }
}
/// What git's stderr actually established, as distinct from how it was worded.
///
/// This layer exists because the wording is upstream's to change and quipu's to
/// live with. Matching text straight to a remedy makes every diagnosis a hostage
/// to a string in someone else's release; naming the EVIDENCE first means a new
/// spelling costs one pattern here and changes nothing downstream, and an
/// unrecognised spelling degrades to `Unrecognised` — which still has a useful
/// answer — instead of falling through to a message that claims nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Evidence {
    /// The name never resolved. Nothing was contacted.
    Dns,
    /// The far end refused the credentials. Only reachable where there are any.
    Auth,
    /// Something answered the port with a refusal — nothing is listening.
    Refused,
    /// A connection was established and then died. The far end IS up.
    Dropped,
    /// No answer at all within the timeout: a route or a firewall.
    TimedOut,
    /// This machine's own git policy forbade the url's protocol.
    PolicyBlocked,
    /// Something answered, and what it said was not the git protocol.
    NotGitProtocol,
    /// git named a reason it would not serve the repository.
    RepositoryNamed,
    /// git refused to serve the repository and named NO reason.
    ///
    /// Its commonest sentence, and the one that carries least: it is what a
    /// refused repository prints AND what a far end that answered and then
    /// failed prints, with nothing in the text to separate them.
    RepositoryUnnamed,
    /// git failed and quipu does not recognise the wording.
    ///
    /// NOT a synonym for "no information". The transport still bounds what can
    /// have gone wrong, and those causes are as true under an unfamiliar
    /// sentence as under a familiar one.
    Unrecognised,
}

/// Sorts git's stderr into what it established. Verbatim strings, ordered by strength.
///
/// Order is evidence strength, not convenience. Authentication is tested before
/// the repository patterns because an ssh refusal prints BOTH "Permission
/// denied" and "Could not read from remote repository", and only the first is
/// true — but it is gated on `transport.authenticates()`, so it can never fire
/// on a protocol with no login to refuse.
///
/// Every pattern here was produced and read, not recalled. Measured on git
/// 2.54.0 and 2.55.0, which emit character-identical text for each of these.
fn evidence_of(lowered: &str, transport: Transport) -> Evidence {
    if lowered.contains("could not resolve hostname")
        || lowered.contains("name or service not known")
    {
        return Evidence::Dns;
    }
    // Local policy, not the far end: nothing was contacted at all. Named before
    // everything else that could be mistaken for a network answer, because it
    // is invisible otherwise — the url is fine and the host may be perfectly up.
    if lowered.contains("not allowed") && lowered.contains("transport") {
        return Evidence::PolicyBlocked;
    }
    if transport.authenticates() && crate::claimref::looks_like_auth(lowered) {
        return Evidence::Auth;
    }
    if lowered.contains("connection refused") {
        return Evidence::Refused;
    }
    // "hung up upon initial contact" is git's OTHER wording for the same event,
    // emitted when it had already seen part of an answer. Both mean the far end
    // was there and stopped talking.
    if lowered.contains("connection reset by peer")
        || lowered.contains("the remote end hung up")
        || lowered.contains("early eof")
    {
        return Evidence::Dropped;
    }
    if lowered.contains("connection timed out") || lowered.contains("no route to host") {
        return Evidence::TimedOut;
    }
    // Something is listening and answering — it is simply not a git daemon.
    // Measured: a socket that replies with anything that is not a pkt-line
    // draws `protocol error: bad line length character: <first four bytes>`.
    if lowered.contains("protocol error") {
        return Evidence::NotGitProtocol;
    }
    if lowered.contains("does not appear to be a git repository")
        || lowered.contains("repository not found")
        || lowered.contains("no such file or directory")
        || lowered.contains("repository not exported")
        || lowered.contains("access denied or repository")
    {
        return Evidence::RepositoryNamed;
    }
    if lowered.contains("could not read from remote repository") {
        return Evidence::RepositoryUnnamed;
    }
    Evidence::Unrecognised
}

/// Turns git's stderr into a cause a person can act on, or names every cause it could be.
///
/// Every branch here is a failure that was met in the field and misread. git
/// reports server-side problems through a client-side error that names the
/// wrong thing: a repository that is present but owned by another user, a
/// daemon whose helpers are not on its PATH, and a repository that genuinely
/// does not exist all arrive as the same sentence. Where the text cannot
/// separate them, this names all of them rather than picking one.
///
/// It takes the URL, not just the text, because text alone is not enough to
/// classify. `access denied` means "your key is not authorised" over ssh and
/// "this repository is absent, unexported, or owned by the wrong user" over
/// `git://` — the same words, different subsystems, and guessing produces
/// exactly the confident misdirection this command exists to end.
///
/// The catch-all is the load-bearing part, and it is deliberately NOT a
/// shrug. git's exact wording is upstream's to change; a diagnosis that
/// evaporates when it does is worse than one that never existed, because it
/// arrives at the moment someone is already lost. So an unrecognised message
/// still gets the causes the TRANSPORT admits — they do not depend on the
/// wording — plus a plain statement that quipu did not recognise the text and
/// that git's own words above are the authority.
fn diagnose_git(url: &str, stderr: &str) -> (String, String) {
    let transport = Transport::of(url);
    let lowered = stderr.to_ascii_lowercase();
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("git failed with no output")
        .to_string();

    match evidence_of(&lowered, transport) {
        Evidence::Dns => (
            format!("the host name does not resolve: {first}"),
            "this is DNS, not the service. Check the spelling of the host, and check that \
             this machine is on the network that resolves it — a name that only exists on a \
             private mesh network does not resolve from outside it."
                .to_string(),
        ),
        Evidence::PolicyBlocked => (
            format!("git on THIS machine refused to use the url's protocol: {first}"),
            "nothing was contacted: this is local policy, not the remote. Some environments \
             disable the plain git protocol wholesale. Check `protocol.<name>.allow` in git \
             config and the GIT_ALLOW_PROTOCOL variable, or point the authority at a \
             protocol this machine permits — ssh carries claims just as well."
                .to_string(),
        ),
        Evidence::Auth => {
            let how = match transport {
                Transport::Http => {
                    "for https: the token or password is wrong, expired, or \
                     belongs to an account without access to this repository. `qp` never \
                     answers a credential prompt, so a missing credential fails here rather \
                     than hanging."
                }
                _ => {
                    "for ssh: the key is not in that account's authorized_keys, or the user \
                     name in the url is not the account that holds it — both fail with the \
                     same message and want different fixes. Confirm the exact user@host the \
                     url names."
                }
            };
            (
                format!("the authority refused authentication: {first}"),
                format!(
                    "the host answered, so this is credentials rather than reachability. {how}"
                ),
            )
        }
        Evidence::Refused => (
            format!("the connection was refused: {first}"),
            "the host is reachable and nothing is listening on that port. THREE causes look \
             identical:\n\
             (1) the service is not running — a daemon started from a login session dies \
             with it;\n\
             (2) the address is stale — a host that moved onto a private mesh network stops \
             answering on its old LAN address;\n\
             (3) this machine, or that one, is locked out of the mesh network — a node can \
             register, obtain a certificate and still be quarantined by the network's access \
             control, and it is then absent from the network's status listing."
                .to_string(),
        ),
        Evidence::Dropped => (
            format!("the authority answered and then dropped the connection: {first}"),
            "something IS listening, so this is not reachability. A git daemon that cannot \
             find its own helper programs accepts the connection and dies before answering, \
             and the client only ever sees the drop. Look at the SERVER's log: it will name \
             the helper it could not run. On a host where git lives outside the default \
             PATH, the daemon needs that directory on its PATH and its exec path set."
                .to_string(),
        ),
        Evidence::NotGitProtocol => (
            format!("something answered, and it was not a git server: {first}"),
            "the port is open and something is talking on it, so this is neither \
             reachability nor permissions — whatever is listening does not speak the git \
             protocol. Usually the port is wrong (another service has it), something sits in \
             front of the daemon and answers first, or the endpoint expects TLS on a port \
             addressed as plaintext. The four characters in git's message are the first \
             bytes of the answer, and they usually identify what really replied."
                .to_string(),
        ),
        Evidence::TimedOut => (
            format!("the connection timed out: {first}"),
            "no answer at all, which is a route or a firewall rather than the service. \
             Check that this machine is on the network the host lives on."
                .to_string(),
        ),
        Evidence::RepositoryNamed => (
            format!("the authority would not serve the repository: {first}"),
            repository_remedy(transport, true),
        ),
        Evidence::RepositoryUnnamed => (
            format!("the authority would not serve the repository, and gave no reason: {first}"),
            repository_remedy(transport, false),
        ),
        Evidence::Unrecognised => (
            format!("git could not use the authority: {first}"),
            format!(
                "quipu does not recognise that message, so git's own words above are the \
                 authority — read them first, and run `git ls-remote <url>` by hand for the \
                 rest of the output. If they do not point somewhere themselves, these are \
                 the causes this transport can produce:\n{}",
                repository_remedy(transport, false)
            ),
        ),
    }
}

/// The causes behind "I will not serve that repository", which differ by transport.
///
/// Ownership leads on every transport because it is the one that costs hours:
/// the repository is present and complete, the client is told it does not
/// exist, and `chmod` does nothing because the check is on the OWNER, not the
/// mode. What differs is the rest of the list — a git daemon has an export flag
/// and no login session, ssh has a login session whose PATH may not carry git,
/// and a local path has neither.
///
/// `named` is whether git gave a reason at all. Measured: a `git://` daemon
/// that accepts a connection and dies before answering prints "Could not read
/// from remote repository" — character for character what it prints when it
/// refuses a repository, on git 2.54.0 and 2.55.0 alike. So when git did NOT
/// name a reason, the daemon itself joins the list of causes; dropping it would
/// be this command quietly picking one of two indistinguishable answers.
fn repository_remedy(transport: Transport, named: bool) -> String {
    let ownership = "(1) OWNERSHIP — a repository owned by a different user than the process \
         serving it is refused by git's dubious-ownership check, and the client is told the \
         repository does not exist. It is present and complete; chmod does nothing, because \
         the check is on the owner and not the mode. Create the repository AS the serving \
         user (inside its own container, if that is where it runs) rather than copying one \
         in, or add the path to that user's safe.directory";
    let absent = "the path is wrong, or the repository was never created. Confirm it is a \
         BARE repository — a working checkout cannot serve pushes";
    // The far end being up and then failing is on the table whenever git named
    // no reason, whatever words it used to not name one.
    // Never on a filesystem path: there is no connection to accept and no
    // process on the other side of one.
    let dead_daemon = if named || transport == Transport::Local {
        String::new()
    } else {
        "\n(+) the far end accepted the connection and FAILED before answering — a daemon \
         that cannot find its own helper programs does exactly this, and git does not \
         distinguish it from the causes above. The SERVER's log is the only place the \
         difference shows"
            .to_string()
    };
    match transport {
        // No authentication exists here, so every cause is about the repository.
        Transport::GitDaemon => format!(
            "the daemon answered, and the git protocol carries NO LOGIN AT ALL — there is no \
             user and no secret anywhere in this request, so nothing here is about \
             permissions. The causes that produce this:\n\
             {ownership};\n\
             (2) the repository is not EXPORTED — a git daemon serves a repository only if it \
             contains `git-daemon-export-ok`, or the daemon was started with --export-all. \
             Touch that file in the repository, or restart the daemon with the flag;\n\
             (3) {absent}{dead_daemon}."
        ),
        Transport::Ssh | Transport::Http => format!(
            "the host answered and did not refuse the credentials, so this is about the \
             repository. The causes that produce this:\n\
             {ownership};\n\
             (2) the server's git is not on the PATH of a non-interactive session, so the \
             helper the connection needs cannot be run. A non-interactive ssh session gets a \
             minimal PATH, and git installed outside it is invisible. Install git where that \
             session looks, or set the PATH the daemon starts with;\n\
             (3) {absent}{dead_daemon}."
        ),
        // A local path has no daemon and no session; only two causes remain.
        Transport::Local => format!(
            "this is a filesystem path, so there is no daemon, no session and no login \
             involved. The causes that produce this:\n\
             {ownership};\n\
             (2) {absent}{dead_daemon}."
        ),
    }
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

    /// The classifier's inputs are git's real sentences, pasted verbatim.
    ///
    /// These are unit tests over text, not over a transport: they prove the
    /// mapping from message to cause, and nothing about whether git produces
    /// that message. The transport-level cases that CAN be produced without a
    /// network live in `tests/remote_doctor.rs`.
    /// Urls that name nobody. RFC 2606 and loopback only.
    const SSH_URL: &str = "ssh://host.invalid/srv/authority.git";
    const GIT_URL: &str = "git://host.invalid/authority.git";
    const HTTPS_URL: &str = "https://host.invalid/authority.git";
    const LOCAL_URL: &str = "/srv/git/authority.git";

    fn remedy_for(url: &str, stderr: &str) -> String {
        diagnose_git(url, stderr).1
    }

    /// Words that can only ever be about a credential.
    ///
    /// A diagnosis containing one of these for a transport that has no
    /// authentication is the bug this list exists to catch: it sends a reader
    /// to inspect an ssh key that took no part in the request.
    fn mentions_authentication(text: &str) -> Option<&'static str> {
        let t = text.to_ascii_lowercase();
        [
            "authentication",
            "credential",
            "authorized_keys",
            "ssh key",
            "password",
            "token",
        ]
        .into_iter()
        .find(|w| t.contains(w))
    }

    #[test]
    fn an_ssh_refusal_is_authentication_and_not_a_missing_repository() {
        let stderr = "git@host.invalid: Permission denied (publickey).\n\
                      fatal: Could not read from remote repository.\n\n\
                      Please make sure you have the correct access rights\nand the repository exists.";
        let (detail, remedy) = diagnose_git(SSH_URL, stderr);
        assert!(detail.contains("authentication"), "{detail}");
        assert!(remedy.contains("authorized_keys"), "{remedy}");
        assert!(
            !remedy.contains("dubious"),
            "an auth refusal must not be diagnosed as ownership: {remedy}"
        );
    }

    /// The bug the coordinator caught against a real `git://` authority.
    ///
    /// git's daemon says "access denied" when a repository is absent,
    /// unexported, or owned by the wrong user. There is no key, no user and no
    /// credential anywhere in a `git://` request, so a diagnosis that names one
    /// is confidently pointing at a subsystem that took no part in the failure
    /// — the same shape as every misdirection this command exists to end.
    #[test]
    fn the_git_protocol_is_never_diagnosed_as_authentication() {
        // Both shapes a git daemon produces for the same three causes.
        for stderr in [
            "fatal: remote error: access denied or repository not exported: /no-such-repo.git",
            "fatal: Could not read from remote repository.\n\n\
             Please make sure you have the correct access rights\nand the repository exists.",
        ] {
            let (detail, remedy) = diagnose_git(GIT_URL, stderr);
            let whole = format!("{detail}\n{remedy}");
            if let Some(word) = mentions_authentication(&whole) {
                panic!("`git://` has no authentication, yet the diagnosis says `{word}`:\n{whole}");
            }
            assert!(remedy.contains("NO LOGIN AT ALL"), "{remedy}");
            // The three causes the message actually means on this transport.
            assert!(remedy.contains("OWNERSHIP"), "{remedy}");
            assert!(remedy.contains("chmod does nothing"), "{remedy}");
            assert!(remedy.contains("git-daemon-export-ok"), "{remedy}");
            assert!(remedy.contains("never created"), "{remedy}");
            // Where git named its reason the list is the three repository
            // causes; where it fell back to its catch-all, a daemon that
            // answered and died prints the SAME sentence and has to be named
            // too. Measured against git itself, not assumed.
            let ambiguous = !stderr.contains("not exported");
            assert_eq!(
                remedy.contains("FAILED before answering"),
                ambiguous,
                "the catch-all sentence is ambiguous and the named one is not: {remedy}"
            );
            // And nothing about a login session's PATH: a daemon has no session.
            assert!(
                !remedy.contains("non-interactive"),
                "a git daemon has no login session to have a PATH: {remedy}"
            );
        }
    }

    #[test]
    fn a_repository_refusal_over_ssh_names_ownership_path_and_absence() {
        let stderr = "fatal: '/srv/git/authority.git' does not appear to be a git repository\n\
                      fatal: Could not read from remote repository.";
        let r = remedy_for(SSH_URL, stderr);
        assert!(r.contains("OWNERSHIP"), "{r}");
        assert!(r.contains("PATH"), "{r}");
        assert!(r.contains("never created"), "{r}");
    }

    /// The identical text, three transports, three different answers.
    ///
    /// This is the whole claim of scheme-aware classification: if these three
    /// ever collapse into one message, the classifier is back to guessing.
    #[test]
    fn the_same_sentence_is_diagnosed_differently_per_transport() {
        let stderr = "fatal: Could not read from remote repository.";
        let ssh = remedy_for(SSH_URL, stderr);
        let git = remedy_for(GIT_URL, stderr);
        let local = remedy_for(LOCAL_URL, stderr);
        assert_ne!(ssh, git);
        assert_ne!(git, local);
        assert_ne!(ssh, local);
        // Only ssh has a login session whose PATH can hide git.
        assert!(ssh.contains("non-interactive"), "{ssh}");
        // Only the daemon has an export flag.
        assert!(git.contains("git-daemon-export-ok"), "{git}");
        // A local path has neither, and no credential either.
        assert!(local.contains("filesystem path"), "{local}");
        assert!(
            mentions_authentication(&local).is_none(),
            "a filesystem path has no credential: {local}"
        );
        // Ownership is the expensive cause and belongs in all three.
        for r in [&ssh, &git, &local] {
            assert!(r.contains("OWNERSHIP"), "{r}");
        }
    }

    #[test]
    fn an_https_refusal_talks_about_tokens_rather_than_authorized_keys() {
        let stderr = "fatal: Authentication failed for 'https://host.invalid/authority.git/'";
        let r = remedy_for(HTTPS_URL, stderr);
        assert!(r.contains("token"), "{r}");
        assert!(
            !r.contains("authorized_keys"),
            "https does not read authorized_keys: {r}"
        );
    }

    #[test]
    fn a_reset_points_at_the_server_and_not_at_the_network() {
        let r = remedy_for(GIT_URL, "fatal: read error: Connection reset by peer");
        assert!(r.contains("helper"), "{r}");
        assert!(
            !r.contains("locked out"),
            "a reset is not a refusal; conflating them sends people to the wrong network: {r}"
        );
    }

    #[test]
    fn a_refusal_names_the_service_the_address_and_the_network_lockout() {
        let r = remedy_for(
            SSH_URL,
            "ssh: connect to host host.invalid port 22: Connection refused",
        );
        assert!(r.contains("not running"), "{r}");
        assert!(r.contains("stale"), "{r}");
        assert!(r.contains("quarantined"), "{r}");
    }

    #[test]
    fn an_unresolvable_host_is_dns_and_says_so() {
        let (detail, remedy) =
            diagnose_git(SSH_URL, "ssh: Could not resolve hostname host.invalid");
        assert!(detail.contains("does not resolve"), "{detail}");
        assert!(remedy.contains("DNS"), "{remedy}");
    }

    #[test]
    fn an_unrecognised_failure_is_passed_through_rather_than_guessed_at() {
        let (detail, remedy) = diagnose_git(SSH_URL, "fatal: something nobody has seen before");
        assert!(
            detail.contains("something nobody has seen before"),
            "{detail}"
        );
        assert!(remedy.contains("by hand"), "{remedy}");
    }

    /// git's exact wording for a connection that dies, measured rather than recalled.
    ///
    /// Each string was produced by running the named git against a loopback
    /// socket that behaves as described, and copied out of its stderr. The
    /// versions are recorded because this text is UPSTREAM'S to change: it is
    /// the input this classifier is least in control of, and the day it changes
    /// the next person needs to know what was true before, and on what.
    ///
    /// Measured 2026-08-28 on git 2.54.0 and git 2.55.0, which emit
    /// character-identical text for every case below. A suspected difference
    /// between those two versions is what prompted the measurement; there is
    /// none, and the reason a `git://` diagnosis can differ between machines
    /// lies in local protocol policy (see `an_environment_that_forbids_the_git_protocol_...`)
    /// rather than in the git version.
    #[test]
    fn the_measured_wordings_for_a_dropped_connection_are_all_recognised() {
        // accept, read the request, close: git 2.54.0 and 2.55.0.
        let read_then_close = "fatal: Could not read from remote repository.\n\n\
                               Please make sure you have the correct access rights\n\
                               and the repository exists.";
        // accept, then reset the connection (SO_LINGER 0): git 2.54.0 and 2.55.0.
        let reset = "fatal: read error: Connection reset by peer";
        // git's other wording for the same event, emitted once it has seen part
        // of an answer.
        let partial = "fatal: the remote end hung up upon initial contact";

        assert_eq!(
            evidence_of(&reset.to_lowercase(), Transport::GitDaemon),
            Evidence::Dropped
        );
        assert_eq!(
            evidence_of(&partial.to_lowercase(), Transport::GitDaemon),
            Evidence::Dropped
        );
        // The catch-all sentence names no reason, so it is not "Dropped" —
        // but the remedy for it must still leave the dropped case on the table.
        assert_eq!(
            evidence_of(&read_then_close.to_lowercase(), Transport::GitDaemon),
            Evidence::RepositoryUnnamed
        );

        // Whichever wording arrives, the reader is sent to the server's log.
        for stderr in [read_then_close, reset, partial] {
            let r = remedy_for(GIT_URL, stderr);
            assert!(
                r.contains("SERVER's log"),
                "a far end that answered and stopped is a server-side problem: {r}"
            );
        }
    }

    /// Something answering that is not a git daemon is its own diagnosis.
    ///
    /// Measured 2026-08-28 on git 2.54.0 and 2.55.0 against a socket that
    /// replies with four bytes of anything: both print this, character for
    /// character. Neither reachability nor permissions — and the old classifier
    /// had no name for it at all.
    #[test]
    fn a_far_end_that_does_not_speak_git_is_told_apart_from_one_that_will_not_serve() {
        let stderr = "fatal: protocol error: bad line length character: hell";
        assert_eq!(
            evidence_of(&stderr.to_lowercase(), Transport::GitDaemon),
            Evidence::NotGitProtocol
        );
        let (detail, remedy) = diagnose_git(GIT_URL, stderr);
        assert!(detail.contains("not a git server"), "{detail}");
        assert!(
            remedy.contains("does not speak the git protocol"),
            "{remedy}"
        );
        // Not the repository, and not the network.
        assert!(!remedy.contains("OWNERSHIP"), "{remedy}");
        assert!(!remedy.contains("not running"), "{remedy}");
        assert!(mentions_authentication(&remedy).is_none(), "{remedy}");
    }

    /// An unfamiliar message must degrade to the transport's causes, not to nothing.
    ///
    /// This is the failure mode the whole `Evidence` layer exists for. git is
    /// free to reword anything at any release; a classifier that answers "no
    /// idea" the moment it does is at its least useful exactly when someone is
    /// most lost. The causes a transport admits do not depend on the wording,
    /// so they survive a rewording that the pattern list does not.
    #[test]
    fn a_message_nobody_has_seen_before_still_names_what_the_transport_admits() {
        let invented = "fatal: something no released git has ever printed";
        assert_eq!(
            evidence_of(invented, Transport::GitDaemon),
            Evidence::Unrecognised
        );
        let (detail, remedy) = diagnose_git(GIT_URL, invented);
        // git's own text is preserved and flagged as the authority.
        assert!(
            detail.contains("no released git has ever printed"),
            "{detail}"
        );
        assert!(remedy.contains("does not recognise"), "{remedy}");
        // And the causes are still there — including the server-side one.
        assert!(remedy.contains("OWNERSHIP"), "{remedy}");
        assert!(remedy.contains("git-daemon-export-ok"), "{remedy}");
        assert!(
            remedy.contains("SERVER's log"),
            "an unrecognised wording must not lose the dead-daemon cause: {remedy}"
        );
        // Still never a credential, on a transport that has none.
        assert!(
            mentions_authentication(&format!("{detail} {remedy}")).is_none(),
            "{remedy}"
        );
        // A filesystem path has no far end, so it must NOT gain that cause.
        let local = remedy_for(LOCAL_URL, invented);
        assert!(
            !local.contains("SERVER's log"),
            "a filesystem path has no daemon to have died: {local}"
        );
    }

    /// Local git policy can forbid a protocol outright, and it looks like nothing else.
    ///
    /// Measured on git 2.54.0 and 2.55.0: `GIT_ALLOW_PROTOCOL` without `git`, or
    /// `protocol.git.allow=never`, both print this. Nothing is contacted, so the
    /// host may be perfectly healthy — and every network-shaped remedy would be
    /// a wild goose chase.
    #[test]
    fn an_environment_that_forbids_the_git_protocol_is_named_as_local_policy() {
        let (detail, remedy) = diagnose_git(GIT_URL, "fatal: transport 'git' not allowed");
        assert!(detail.contains("THIS machine"), "{detail}");
        assert!(remedy.contains("GIT_ALLOW_PROTOCOL"), "{remedy}");
        assert!(remedy.contains("protocol."), "{remedy}");
        assert!(
            remedy.contains("nothing was contacted"),
            "the host is not implicated and the remedy must say so: {remedy}"
        );
        // Not a repository problem, and not a network one.
        assert!(!remedy.contains("OWNERSHIP"), "{remedy}");
        assert!(!remedy.contains("not running"), "{remedy}");
    }

    #[test]
    fn every_url_form_is_sorted_into_the_transport_that_produced_it() {
        assert_eq!(Transport::of(SSH_URL), Transport::Ssh);
        // scp-like syntax is ssh too, and it is the form people actually type.
        assert_eq!(
            Transport::of("git@host.invalid:authority.git"),
            Transport::Ssh
        );
        assert_eq!(Transport::of(GIT_URL), Transport::GitDaemon);
        assert_eq!(Transport::of(HTTPS_URL), Transport::Http);
        assert_eq!(Transport::of(LOCAL_URL), Transport::Local);
        assert_eq!(Transport::of("./authority.git"), Transport::Local);
        // Only these two carry a credential at all.
        assert!(Transport::Ssh.authenticates() && Transport::Http.authenticates());
        assert!(!Transport::GitDaemon.authenticates() && !Transport::Local.authenticates());
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
}
