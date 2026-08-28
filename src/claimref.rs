//! Remote claim arbitration: the git ref a claim must hold before it may be recorded.
//!
#![doc = include_str!("../docs/modules/claimref.md")]

use crate::{db, remote};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Namespace for claim refs.
///
/// Outside `refs/heads` and `refs/tags`, so it is invisible to every forge web
/// UI while staying readable over the wire.
pub const REF_PREFIX: &str = "refs/quipu/claims/";

/// `meta` key prefix caching the blob sha this store pushed for a held ref.
///
/// Local bookkeeping, not state: losing it costs the lease on the delete, not
/// the ability to release.
const SHA_KEY_PREFIX: &str = "remote.claimsha.";

/// The bare repository quipu pushes claim blobs out of: `<store-dir>/claims.git`.
///
/// Its own repository rather than the user's checkout, because
/// `git hash-object -w` writes into whatever object store it is pointed at and
/// quipu has no business putting objects in a repo it does not own — nor any
/// guarantee it is being run inside one.
fn repo_dir(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("claims.git")
}

fn ensure_repo(db_path: &Path) -> Result<PathBuf> {
    let dir = repo_dir(db_path);
    if dir.join("HEAD").exists() {
        return Ok(dir);
    }
    let ok = std::process::Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| unreachable_err(&format!("could not run git: {e}")))?
        .success();
    if !ok {
        return Err(unreachable_err(&format!(
            "could not create quipu's claim repository at {}",
            dir.display()
        )));
    }
    Ok(dir)
}

/// The opaque name of one ticket's claim ref, derived from its canonical identity.
///
/// The inputs are the *origin* store's uuid and the rowid the ticket holds
/// there — `task_origin`'s `(origin_uuid, origin_task_id)`, which is the one
/// name a ticket has everywhere in the world. Nothing machine-local may go in.
/// Peers are not replicas: they keep their own `project_uuid`, and the same
/// logical ticket is numbered differently in each store, so a ref keyed on the
/// asking store's uuid or on a display id makes two peers compute two different
/// names, contend over nothing, and both win. Measured: two stores, one
/// authority, two refs, both claims exit 0.
///
/// Hashed rather than spelled out because a custom ref is anonymously readable
/// by anyone who can reach the authority: `refs/quipu/claims/QP-42` would
/// publish the ticket numbering and the shape of the backlog to a host that is
/// only supposed to referee. The uuid in the digest is also what keeps the
/// small id space from being enumerated.
pub fn ref_name(origin_uuid: &str, origin_task_id: i64) -> String {
    let digest =
        sha256_hex(format!("quipu-claim\u{0}{origin_uuid}\u{0}{origin_task_id}").as_bytes());
    format!("{REF_PREFIX}{}", &digest[..40])
}

/// The `(origin_uuid, origin_task_id)` pair naming `task_id` anywhere in the world.
///
/// A row in `task_origin` means the task was minted elsewhere and carries that
/// store's name for it. No row means it was minted here, and its canonical name
/// is this store's uuid plus its own rowid — which is why `qp add` writes
/// nothing to that table and never has to.
pub fn canonical_id(conn: &Connection, task_id: i64) -> Result<(String, i64)> {
    let found: Option<(String, i64)> = conn
        .query_row(
            "SELECT origin_uuid, origin_task_id FROM task_origin WHERE task_id = ?1",
            [task_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match found {
        Some(pair) => Ok(pair),
        None => Ok((project_uuid(conn)?, task_id)),
    }
}

fn sha_key(refname: &str) -> String {
    format!("{SHA_KEY_PREFIX}{}", &refname[REF_PREFIX.len()..])
}

/// The store's identity, which salts every ref name.
pub fn project_uuid(conn: &Connection) -> Result<String> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'project_uuid'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    v.ok_or_else(|| db::invalid_input("store has no project_uuid; run `qp init` first"))
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

/// The authority answered, and the answer was "someone else holds this".
fn held_err(display_id: &str) -> anyhow::Error {
    db::conflict(
        "claim_held_remotely",
        format!(
            "{display_id} is already claimed: the claim ref exists on the authority. \
             Nothing was changed locally."
        ),
        Some(display_id.to_string()),
    )
}

/// The authority could not be reached.
///
/// Distinct from "already claimed" on purpose — conflating the two is how a
/// caller retries a dead host forever.
fn unreachable_err(detail: &str) -> anyhow::Error {
    db::conflict(
        "authority_unreachable",
        format!(
            "could not reach the claim authority ({detail}); no local state was changed. \
             Check it with `qp remote show --check`, or run `qp remote clear --authority` \
             to return this store to local-only."
        ),
        None,
    )
}

/// The authority was reached and refused the credentials. Retrying will not help.
fn auth_err(detail: &str) -> anyhow::Error {
    db::conflict(
        "authority_auth_failed",
        format!(
            "the claim authority rejected authentication ({detail}); no local state was \
             changed. Fix the credentials for that url — retrying will not help."
        ),
        None,
    )
}

/// Best-effort split of the exit-128 bucket into auth-failed and unreachable.
///
/// A miss falls to `unreachable`, never to "already claimed": the 1-vs-128
/// split is the only classification allowed to decide whether a ticket is held.
pub fn looks_like_auth(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    [
        "permission denied",
        "authentication failed",
        "publickey",
        "403 forbidden",
        "access denied",
        "could not read username",
        "could not read password",
        "invalid username or password",
        "terminal prompts disabled",
    ]
    .iter()
    .any(|p| s.contains(p))
}

// ---------------------------------------------------------------------------
// the arbiter
// ---------------------------------------------------------------------------

/// One store's connection to its claim authority, for the duration of one command.
///
/// `lane` is the acting agent's id and is passed straight to
/// `remote::ssh_command`: sequential claims by one agent then share an ssh
/// master, while a wave fanning out gets one socket per agent instead of
/// bursting a single master past sshd's `MaxSessions`. Passing a constant here
/// reintroduces a measured, total failure of every claim in a fan-out.
pub struct Arbiter {
    db_path: PathBuf,
    url: String,
    lane: String,
}

impl Arbiter {
    pub fn new(db_path: &Path, url: String, lane: &str) -> Self {
        Self {
            db_path: db_path.to_path_buf(),
            url,
            lane: lane.to_string(),
        }
    }

    /// Reads the configured authority, or `None` for a local-only store.
    ///
    /// One `SELECT` on the unconfigured path, and nothing else: a store with no
    /// authority must not pay for the existence of this module.
    pub fn for_store(db_path: &Path, conn: &Connection, lane: &str) -> Result<Option<Self>> {
        Ok(remote::authority(conn)?.map(|url| Self::new(db_path, url, lane)))
    }

    fn git(&self, repo: &Path) -> std::process::Command {
        let mut c = remote::git(&self.db_path, &self.url, &self.lane);
        c.arg("--git-dir").arg(repo);
        c
    }

    /// Writes the claim object and returns its sha.
    ///
    /// A **blob**, never a commit. A commit carries the operator's name and
    /// email from git config — real identity, published to a third-party host —
    /// plus a parent pointer whose chain can drag ledger objects along with the
    /// push. A blob has no author, no committer, no tree and no parent: its
    /// bytes are exactly what is written here and nothing is reachable from it.
    ///
    /// The `nonce` is not decoration. If two claimants push the *same* object,
    /// git answers `Everything up-to-date` with exit 0 and tells both of them
    /// they won — measured, five false winners out of ten. Every attempt must
    /// mint an object no other attempt can produce.
    fn write_blob(&self, repo: &Path, agent: &str) -> Result<String> {
        let body = serde_json::json!({
            "agent": agent,
            "ts": crate::time::now_rfc3339(),
            "nonce": uuid::Uuid::new_v4().to_string(),
        })
        .to_string();
        let mut child = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(repo)
            .args(["hash-object", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| unreachable_err(&format!("could not run git: {e}")))?;
        child
            .stdin
            .take()
            .expect("stdin piped")
            .write_all(body.as_bytes())
            .map_err(|e| unreachable_err(&format!("could not write the claim object: {e}")))?;
        let out = child
            .wait_with_output()
            .map_err(|e| unreachable_err(&format!("git hash-object failed: {e}")))?;
        if !out.status.success() {
            return Err(unreachable_err("git could not write the claim object"));
        }
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if sha.len() < 40 {
            return Err(unreachable_err("git returned no object id for the claim"));
        }
        Ok(sha)
    }

    /// Acquires `refname` on the authority, or fails without touching anything.
    ///
    /// The lease with an **empty** expected value —
    /// `--force-with-lease=<ref>:` — is the whole mechanism: it means "this ref
    /// must not already exist", and the remote is what verifies it. A plain
    /// push is not a substitute and must never be used here. Refs outside
    /// `refs/heads` and `refs/tags` accept any fast-forward, so a second
    /// claimant whose object *descends* from the winner's is accepted with exit
    /// 0 and silently steals the claim.
    ///
    /// Returns the sha of the object now sitting at that ref.
    pub fn acquire(&self, refname: &str, agent: &str, display_id: &str) -> Result<String> {
        let repo = ensure_repo(&self.db_path)?;
        let sha = self.write_blob(&repo, agent)?;
        let out = self
            .git(&repo)
            .arg("push")
            .arg(format!("--force-with-lease={refname}:"))
            .arg(&self.url)
            .arg(format!("{sha}:{refname}"))
            .output()
            .map_err(|e| unreachable_err(&format!("could not run git: {e}")))?;
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        match out.status.code() {
            // Exit 0 is a win — unless git had nothing to do, which means the
            // ref already held this exact object. Treated as a loss: a push
            // that updated nothing arbitrated nothing.
            Some(0) => {
                if format!("{stdout}{stderr}").contains("Everything up-to-date") {
                    Err(held_err(display_id))
                } else {
                    Ok(sha)
                }
            }
            // Measured, and mechanical: the lease was refused, so the ref exists.
            Some(1) => Err(held_err(display_id)),
            // 128 is "never got an answer" — unreachable host or refused
            // credentials. Fail closed: no local mutation, and never a
            // local-only fallback.
            _ => {
                if looks_like_auth(&stderr) {
                    Err(auth_err(first_line(&stderr)))
                } else {
                    Err(unreachable_err(first_line(&stderr)))
                }
            }
        }
    }

    /// Creates and then deletes a probe ref, proving the namespace is writable.
    ///
    /// A read probe proves nothing here. `git ls-remote` succeeds against a
    /// repository whose refs the caller may not write, and against one served
    /// read-only — and a claim is a ref CREATION, so a store that passes a read
    /// check can still fail every claim it ever makes. This is the only probe
    /// that exercises the operation `claim` performs.
    ///
    /// Deliberately not routed through `acquire`/`release`: those are the claim
    /// path and answer in the claim vocabulary ("already claimed"), which is
    /// the wrong sentence for a diagnostic. The failure this must report is
    /// "the namespace refused a write", and the leftover case — created but not
    /// removed — has to be named rather than swallowed, because it leaves
    /// something behind on someone else's host.
    pub fn probe_namespace(&self, refname: &str) -> WriteProbe {
        let repo = match ensure_repo(&self.db_path) {
            Ok(r) => r,
            Err(e) => return WriteProbe::CreateFailed(e.to_string()),
        };
        let sha = match self.write_blob(&repo, "qp-remote-doctor") {
            Ok(s) => s,
            Err(e) => return WriteProbe::CreateFailed(e.to_string()),
        };
        let created = self
            .git(&repo)
            .arg("push")
            .arg(format!("--force-with-lease={refname}:"))
            .arg(&self.url)
            .arg(format!("{sha}:{refname}"))
            .output();
        match created {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                return WriteProbe::CreateFailed(
                    first_line(&String::from_utf8_lossy(&o.stderr)).to_string(),
                )
            }
            Err(e) => return WriteProbe::CreateFailed(format!("could not run git: {e}")),
        }
        // Unleased: this process created the ref one round trip ago and is the
        // only thing that has ever known its name, so a lease here would only
        // add a way to leave litter behind.
        if self.release(refname, None) {
            WriteProbe::Ok
        } else {
            WriteProbe::Leftover {
                refname: refname.to_string(),
            }
        }
    }

    /// Deletes `refname`, freeing the ticket for the next claimant.
    ///
    /// Best-effort by design, and it runs *after* the local release has
    /// committed. The two orders are not symmetric: acquiring before the local
    /// write means a failure leaves the ticket un-held everywhere, while
    /// releasing locally before deleting the ref means a failure leaves it held
    /// by a store that no longer wants it — recoverable, and never two holders.
    ///
    /// `expect` leases the delete against the sha this store pushed, so a
    /// late release cannot remove a ref some other claimant now owns. `None`
    /// (the forced path, and the path when the cached sha is gone) deletes
    /// unconditionally.
    pub fn release(&self, refname: &str, expect: Option<&str>) -> bool {
        let Ok(repo) = ensure_repo(&self.db_path) else {
            return false;
        };
        let mut c = self.git(&repo);
        c.arg("push");
        if let Some(sha) = expect {
            c.arg(format!("--force-with-lease={refname}:{sha}"));
        }
        c.arg(&self.url).arg(format!(":{refname}"));
        matches!(c.output(), Ok(o) if o.status.success())
    }
}

/// What a namespace write probe found. `Leftover` is a failure with a mess attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteProbe {
    Ok,
    /// The ref could not be created. Carries git's own first line.
    CreateFailed(String),
    /// Created but not removed — the caller must name the ref so a human can.
    Leftover {
        refname: String,
    },
}

fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no output from git")
}

// ---------------------------------------------------------------------------
// the two edges, as the commands use them
// ---------------------------------------------------------------------------

/// Acquires the ref for `display_id`, returning what a rollback would need.
///
/// Call this *before* the local transaction. The order is the entire point of
/// the design: acquire remotely, then record locally. Reversed, a store writes
/// down a claim it does not hold.
pub fn acquire_for(
    db_path: &Path,
    conn: &Connection,
    task_id: i64,
    display_id: &str,
    agent: &str,
) -> Result<Option<Held>> {
    let Some(arb) = Arbiter::for_store(db_path, conn, agent)? else {
        return Ok(None);
    };
    let (uuid, origin_task_id) = canonical_id(conn, task_id)?;
    let refname = ref_name(&uuid, origin_task_id);
    let sha = arb.acquire(&refname, agent, display_id)?;
    Ok(Some(Held { arb, refname, sha }))
}

/// A ref this process acquired and is therefore responsible for.
pub struct Held {
    arb: Arbiter,
    pub refname: String,
    pub sha: String,
}

impl Held {
    /// Records the pushed sha so a later release can lease its delete.
    pub fn remember(&self, conn: &Connection) -> Result<()> {
        remote::set(conn, &sha_key(&self.refname), &self.sha)
    }

    /// Gives the ref back after the local transaction refused the claim.
    ///
    /// Without this, a ticket whose local guard rejected the claim stays held
    /// on the authority by a store that never started the work.
    pub fn roll_back(&self) {
        if !self.arb.release(&self.refname, Some(&self.sha)) {
            eprintln!(
                "warning: the local claim failed but the claim ref could not be released; \
                 the ticket may stay held on the authority"
            );
        }
    }
}

/// Everything a release edge needs, resolved before its local transaction.
///
/// `None` for a local-only store — and also `None` when this store holds no
/// claim ref for the ticket, which is the common case for `cancel` and
/// `reclaim` on work that was never claimed. Holding the ref is recorded when
/// it is acquired, so its absence is a local answer: no probe, no push, no
/// network at all on a path that has nothing to release.
pub fn release_for(
    db_path: &Path,
    conn: &Connection,
    task_id: i64,
    lane: &str,
) -> Result<Option<Release>> {
    let Some(arb) = Arbiter::for_store(db_path, conn, lane)? else {
        return Ok(None);
    };
    let (uuid, origin_task_id) = canonical_id(conn, task_id)?;
    let refname = ref_name(&uuid, origin_task_id);
    let Some(expect) = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [sha_key(&refname)],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    Ok(Some(Release {
        arb,
        refname,
        expect,
    }))
}

/// A pending ref deletion, run once the local release has committed.
pub struct Release {
    arb: Arbiter,
    refname: String,
    expect: String,
}

impl Release {
    /// Deletes the ref and drops the cached sha. Never fails the command.
    ///
    /// The delete is leased against the sha this store pushed. If the lease is
    /// refused the ref is left alone rather than forced: a refused lease means
    /// the ref is either already gone or now holds someone else's claim, and
    /// forcing would delete a claim that is being honoured. The cached sha is
    /// dropped either way — this store is done with the ref regardless of what
    /// the authority now says about it.
    pub fn finish(&self, conn: &Connection) {
        if !self.arb.release(&self.refname, Some(&self.expect)) {
            eprintln!(
                "warning: released locally, but the claim ref could not be deleted on the \
                 authority; the ticket may stay held there"
            );
        }
        let _ = remote::clear(conn, &sha_key(&self.refname));
    }
}

// ---------------------------------------------------------------------------
// sha-256
// ---------------------------------------------------------------------------

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256, hand-rolled rather than pulled in as a crate.
///
/// Sixty lines against a dependency, on a project with a stated leanness
/// budget. It is used for one thing — naming a ref so the name says nothing —
/// and `sha256_matches_the_published_vectors` pins it to the standard test
/// vectors, which is what makes "hand-rolled" safe to say.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        let (mut e, mut f, mut g, mut hh) = (h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (dst, src) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *dst = dst.wrapping_add(src);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_published_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn a_ref_name_reveals_nothing_about_the_ticket() {
        let name = ref_name("11111111-2222-3333-4444-555555555555", 42);
        assert!(name.starts_with(REF_PREFIX));
        assert!(!name.contains("42"), "the ticket number must not survive");
        assert_eq!(name.len(), REF_PREFIX.len() + 40);
    }

    /// The name is a function of the canonical identity and of nothing else.
    ///
    /// The equality is the load-bearing half: two peers numbering the same
    /// ticket differently still arrive at one name, because neither the asking
    /// store's uuid nor its local numbering is an input.
    #[test]
    fn ref_names_follow_the_origin_and_never_the_asking_store() {
        let origin = "11111111-2222-3333-4444-555555555555";
        let other = "99999999-2222-3333-4444-555555555555";
        assert_eq!(ref_name(origin, 1), ref_name(origin, 1));
        assert_ne!(ref_name(origin, 1), ref_name(other, 1));
        assert_ne!(ref_name(origin, 1), ref_name(origin, 2));
    }

    /// The `--as` value reaches ssh as the multiplexing lane.
    ///
    /// Not cosmetic, and not covered by the equivalent test one layer down: a
    /// constant passed *here* would make every claim in a fan-out share one
    /// ControlMaster and fail with errors that read as the authority being
    /// down. This asserts the value actually travels.
    #[test]
    fn the_acting_agent_is_the_ssh_lane() {
        if std::env::var_os("GIT_SSH_COMMAND").is_some()
            || std::env::var_os("GIT_SSH").is_some()
            || std::env::var_os("QUIPU_SSH_MULTIPLEX").is_some()
        {
            return; // the injection is switched off; the assertions would be vacuous
        }
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let url = "user@host.invalid:authority.git".to_string();
        let ssh = |lane: &str| -> String {
            let arb = Arbiter::new(&db, url.clone(), lane);
            let c = arb.git(Path::new("repo.git"));
            c.get_envs()
                .find(|(k, _)| *k == std::ffi::OsStr::new("GIT_SSH_COMMAND"))
                .and_then(|(_, v)| v)
                .expect("an ssh url must get a GIT_SSH_COMMAND")
                .to_string_lossy()
                .into_owned()
        };
        assert_ne!(
            ssh("agent-a"),
            ssh("agent-b"),
            "two agents must not share one ControlMaster"
        );
        assert_eq!(
            ssh("agent-a"),
            ssh("agent-a"),
            "one agent reuses its socket"
        );
    }

    #[test]
    fn a_missed_stderr_match_falls_to_unreachable_never_to_held() {
        assert!(looks_like_auth("fatal: Authentication failed for 'x'"));
        assert!(looks_like_auth("git@host: Permission denied (publickey)."));
        assert!(!looks_like_auth(
            "fatal: could not read from remote repository"
        ));
    }
}
