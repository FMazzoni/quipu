//! `qp sync` — the explicit ledger exchange between a store and its remote.
//!
#![doc = include_str!("../../docs/modules/sync.md")]

use crate::cmd::{export, import};
use crate::outcome::{emit, Outcome};
use crate::{db, remote};
use anyhow::{Context, Result};
use clap::Args;
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The single branch the ledger lives on, on both ends of the wire.
///
/// Named rather than inherited from the remote's `HEAD` so a bare repo created
/// with any default branch works, and so the refspecs below can be explicit —
/// which is what keeps `refs/quipu/claims/*` out of reach.
pub const LEDGER_BRANCH: &str = "quipu-ledger";

/// ssh multiplexing lane for every git child this command spawns.
const LANE: &str = "ledger-sync";

/// How many times a push may lose a race with another store before giving up.
const PUSH_ATTEMPTS: u32 = 3;

/// Author and committer on ledger commits.
///
/// Deliberately not the user's git identity: these commits are machine
/// artifacts in a repository the user never authors in, and borrowing their
/// name would put it in a history they did not write.
const COMMIT_NAME: &str = "quipu";
const COMMIT_EMAIL: &str = "quipu@quipu.invalid";

#[derive(Args, Debug)]
pub struct SyncArgs {
    /// Emit JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

/// One exchange: what went out, what came back, and what the merge did with it.
#[derive(Serialize)]
pub struct SyncOutcome {
    ledger: String,
    dir: String,
    branch: &'static str,
    /// Whether this run wrote to the remote. False on a second sync with nothing new.
    pushed: bool,
    export: export::ExportOutcome,
    /// Batch files that arrived from this project's other stores, `<uuid>/<file>`.
    ///
    /// Batches belonging to other projects on the same ledger are fetched into
    /// the clone as well — everyone holds the whole repository — but they are
    /// not this store's to replay and are not counted here.
    fetched: Vec<String>,
    /// Absent when nothing new arrived — the merge is skipped rather than replayed for nothing.
    import: Option<import::ImportOutcome>,
}

impl Outcome for SyncOutcome {
    fn human(&self) -> String {
        let mut lines = Vec::new();
        match self.export.event_count {
            0 => lines.push("nothing new to publish".to_string()),
            n => lines.push(format!(
                "published {n} {}",
                if n == 1 { "event" } else { "events" }
            )),
        }
        if self.pushed {
            lines.push(format!("pushed {} to {}", self.branch, self.ledger));
        }
        match &self.import {
            None => lines.push("nothing new from the remote".to_string()),
            Some(i) => lines.push(format!(
                "fetched {} peer {} and merged {} {}",
                self.fetched.len(),
                if self.fetched.len() == 1 {
                    "batch"
                } else {
                    "batches"
                },
                i.events_new,
                if i.events_new == 1 { "event" } else { "events" }
            )),
        }
        lines.join("\n")
    }
}

pub fn run(db_path: &Path, a: SyncArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let url = remote::load(&conn)?.ledger.ok_or_else(|| {
        db::invalid_input(
            "this store has no ledger remote; run `qp remote set --ledger <url>` first. \
             Sync never guesses a destination, and the claim authority is not one — it is \
             a separate url and sync must never read or write it.",
        )
    })?;
    let uuid: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'project_uuid'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let uuid =
        uuid.ok_or_else(|| db::invalid_input("store has no project_uuid; run `qp init` first"))?;
    let key = crate::project::require(&conn)?;
    drop(conn);
    remote::validate_url(&url)?;

    let dir = ledger_dir(db_path);
    ensure_repo(&dir, &url)?;
    // A crash between `export` and `commit` on an earlier run leaves batches in
    // the tree that git has never seen, and `git merge` refuses to run over
    // them. Adopting them first is also correct on its own terms: they are
    // immutable files this store already wrote.
    commit_pending(&dir)?;

    let before = peer_batches(&dir, &uuid, &key)?;
    fetch(db_path, &dir, &url)?;
    integrate(&dir)?;

    let exported = export::export(db_path, &dir)?;
    commit_pending(&dir)?;
    let pushed = push(db_path, &dir, &url)?;

    // After the push, not before: a lost race re-fetches, and the batches that
    // arrived on the retry are as new to this store as the first fetch's were.
    let after = peer_batches(&dir, &uuid, &key)?;
    let fetched: Vec<String> = after.difference(&before).cloned().collect();

    let imported = if fetched.is_empty() {
        None
    } else {
        Some(import::import(
            db_path,
            &import::ImportArgs {
                from: dir.clone(),
                restore: false,
                merge: true,
                force: false,
                json: false,
            },
        )?)
    };

    emit(
        a.json,
        &SyncOutcome {
            ledger: url,
            dir: dir.display().to_string(),
            branch: LEDGER_BRANCH,
            pushed,
            export: exported,
            fetched,
            import: imported,
        },
    )
}

/// Brings a remote ledger onto this machine and reports where it landed.
///
/// The joining path (`qp init --from <url>`) and the guard that reads
/// `$QUIPU_LEDGER` both need a ledger they can *read* before a store exists, and
/// a url is what someone setting up a new machine actually has. This is that
/// step, and it is deliberately the same `ensure_repo` / `fetch` / `integrate`
/// the exchange below runs — a second fetch path would be a second set of
/// refspecs to get wrong, and the refspec is what keeps `refs/quipu/claims/*`
/// out of reach.
///
/// It never pushes and never writes to the store: reading a ledger is not
/// joining one, and `qp init` decides that afterwards.
pub fn materialise(db_path: &Path, url: &str) -> Result<PathBuf> {
    remote::validate_url(url)?;
    let dir = ledger_dir(db_path);
    ensure_repo(&dir, url)?;
    fetch(db_path, &dir, url)?;
    integrate(&dir)?;
    Ok(dir)
}

/// The ledger clone: `<store-dir>/ledger`, beside the database.
///
/// Inside the store directory because that directory is already ignored by the
/// project's git — a ledger that landed in the project tree would eventually be
/// committed, which is the failure this placement exists to prevent.
pub fn ledger_dir(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("ledger")
}

// ---------------------------------------------------------------------------
// git
// ---------------------------------------------------------------------------

/// A `git -C <dir>` carrying quipu's ssh environment and commit identity.
fn git(db_path: Option<&Path>, dir: &Path, url: &str) -> Command {
    let mut c = match db_path {
        Some(p) => remote::git(p, url, LANE),
        None => {
            let mut c = Command::new("git");
            c.env("GIT_TERMINAL_PROMPT", "0");
            c
        }
    };
    c.arg("-C").arg(dir);
    c.env("GIT_AUTHOR_NAME", COMMIT_NAME)
        .env("GIT_AUTHOR_EMAIL", COMMIT_EMAIL)
        .env("GIT_COMMITTER_NAME", COMMIT_NAME)
        .env("GIT_COMMITTER_EMAIL", COMMIT_EMAIL);
    c
}

/// Runs a git child and returns its stdout, or an error carrying its stderr.
fn run_git(mut c: Command, what: &str) -> Result<String> {
    let out = c
        .output()
        .with_context(|| format!("running git to {what}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(anyhow::anyhow!("could not {what}: {err}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether a git child succeeded, discarding both streams.
fn git_ok(mut c: Command) -> bool {
    matches!(c.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status(), Ok(s) if s.success())
}

/// Creates the ledger clone if it is missing and points it at `url`.
///
/// `git init` plus a remote rather than `git clone`: the remote is legitimately
/// empty on the first sync of the first machine, and a clone of an empty
/// repository leaves a repo with no branch to push anyway.
fn ensure_repo(dir: &Path, url: &str) -> Result<()> {
    if !dir.join(".git").exists() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating ledger clone {}", dir.display()))?;
        let mut c = git(None, dir, url);
        c.args(["init", "-q"]);
        run_git(c, "create the ledger clone")?;
        let mut c = git(None, dir, url);
        c.args(["symbolic-ref", "HEAD"])
            .arg(format!("refs/heads/{LEDGER_BRANCH}"));
        run_git(c, "name the ledger branch")?;
    }
    let mut set = git(None, dir, url);
    set.args(["remote", "set-url", "origin", url]);
    if !git_ok(set) {
        let mut add = git(None, dir, url);
        add.args(["remote", "add", "origin", url]);
        run_git(add, "attach the ledger clone to the remote")?;
    }
    Ok(())
}

/// Commits whatever is in the tree, if anything. Returns whether it committed.
fn commit_pending(dir: &Path) -> Result<bool> {
    let mut add = git(None, dir, "");
    add.args(["add", "-A"]);
    run_git(add, "stage the ledger")?;

    let mut clean = git(None, dir, "");
    clean.args(["diff", "--cached", "--quiet"]);
    if git_ok(clean) {
        return Ok(false);
    }
    let mut commit = git(None, dir, "");
    commit.args(["commit", "-q", "-m", "quipu export"]);
    run_git(commit, "commit the ledger")?;
    Ok(true)
}

/// Fetches the remote's branches — and only its branches.
///
/// The refspec is `refs/heads/*` and nothing else. `refs/quipu/claims/*` is the
/// authority's namespace and a second copy of it is a second referee, so it is
/// never named here, and no `--mirror`, `--all` or tag-following flag is
/// allowed to widen this by accident.
fn fetch(db_path: &Path, dir: &Path, url: &str) -> Result<()> {
    let mut c = git(Some(db_path), dir, url);
    c.args([
        "fetch",
        "--no-tags",
        "origin",
        "+refs/heads/*:refs/remotes/origin/*",
    ]);
    run_git(c, "reach the ledger remote").map(|_| ())
}

fn rev_parse(dir: &Path, rev: &str) -> Option<String> {
    let mut c = git(None, dir, "");
    c.args(["rev-parse", "--verify", "-q", rev]);
    let out = c.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Folds the fetched branch into the local one.
///
/// Divergence is the normal case, not an error path: two machines that both
/// worked offline both have commits the other does not. It merges cleanly with
/// no strategy option because batch filenames carry their origin store's uuid
/// and a batch is never rewritten — so the two trees only ever *add* files, and
/// a union of adds has nothing to reconcile. A conflict here means that
/// invariant broke, so it aborts rather than inventing a resolution.
fn integrate(dir: &Path) -> Result<()> {
    let remote_ref = format!("refs/remotes/origin/{LEDGER_BRANCH}");
    if rev_parse(dir, &remote_ref).is_none() {
        return Ok(()); // remote has no ledger yet; this store is the first
    }
    if rev_parse(dir, "HEAD").is_none() {
        let mut c = git(None, dir, "");
        c.args(["reset", "--hard", "-q", &remote_ref]);
        run_git(c, "adopt the remote ledger")?;
        return Ok(());
    }
    let mut c = git(None, dir, "");
    c.args([
        "merge",
        "--no-edit",
        "--allow-unrelated-histories",
        "-q",
        &remote_ref,
    ]);
    if run_git(c, "merge the remote ledger").is_err() {
        let mut abort = git(None, dir, "");
        abort.args(["merge", "--abort"]);
        let _ = git_ok(abort);
        return Err(anyhow::anyhow!(
            "the ledger merge conflicted, which should be impossible: batches are named \
             after the store that wrote them and are never rewritten. Something outside \
             quipu edited the ledger. Nothing was changed."
        ));
    }
    Ok(())
}

/// Publishes the local branch, re-integrating and retrying if a peer got there first.
///
/// Returns whether anything was actually sent. A push is skipped outright when
/// the local branch already matches the fetched remote one, which is what makes
/// a second sync with nothing new touch nothing at all.
fn push(db_path: &Path, dir: &Path, url: &str) -> Result<bool> {
    for attempt in 0..PUSH_ATTEMPTS {
        let Some(head) = rev_parse(dir, "HEAD") else {
            return Ok(false); // nothing has ever been exported
        };
        if rev_parse(dir, &format!("refs/remotes/origin/{LEDGER_BRANCH}")).as_deref() == Some(&head)
        {
            return Ok(false);
        }
        let mut c = git(Some(db_path), dir, url);
        c.args(["push", "-q", "origin"]).arg(format!(
            "refs/heads/{LEDGER_BRANCH}:refs/heads/{LEDGER_BRANCH}"
        ));
        if git_ok(c) {
            adopt_head(url);
            // Keep the remote-tracking ref honest, so the next sync's
            // already-in-sync check compares against what is really there.
            let mut u = git(None, dir, "");
            u.args([
                "update-ref",
                &format!("refs/remotes/origin/{LEDGER_BRANCH}"),
            ])
            .arg(&head);
            let _ = git_ok(u);
            return Ok(true);
        }
        if attempt + 1 < PUSH_ATTEMPTS {
            fetch(db_path, dir, url)?;
            integrate(dir)?;
        }
    }
    Err(anyhow::anyhow!(
        "the ledger remote kept moving while pushing; nothing local was lost, run \
         `qp sync` again"
    ))
}

/// Points a filesystem ledger's `HEAD` at the branch the batches are on.
///
/// A bare repository is created with `HEAD` on `main`/`master`, and pushing the
/// first branch does NOT move it — measured, not assumed. So a ledger nobody has
/// touched by hand has a `HEAD` naming a branch that does not exist, and
/// `git clone <ledger>` yields an EMPTY working tree with a
/// `remote HEAD refers to nonexistent ref` warning most people scroll past. The
/// batches are all there, on `quipu-ledger`, and everything downstream then
/// reports "no project here" about a ledger that is full.
///
/// Only ever fired when `HEAD` does not resolve — a repository whose `HEAD` is a
/// real branch is being used for something else, and quipu does not get to
/// repoint it. And only where the ledger is a directory this process can open:
/// git has no wire command for setting a remote's `HEAD`, so over ssh or https
/// this cannot be done at all and the message in `cmd/init.rs` carries the
/// explanation instead.
///
/// Best effort throughout. Failing to improve someone's clone ergonomics must
/// never fail their sync.
fn adopt_head(url: &str) {
    let Some(path) = remote::local_path(url) else {
        return;
    };
    if rev_parse(&path, "HEAD").is_some() {
        return;
    }
    let mut c = git(None, &path, "");
    c.args(["symbolic-ref", "HEAD"])
        .arg(format!("refs/heads/{LEDGER_BRANCH}"));
    let _ = git_ok(c);
}

/// Every batch in the clone that another store of THIS project wrote, as `<uuid>/<file>`.
///
/// Our own batches are excluded because they are already in this store's log;
/// re-importing them is work with no effect. The difference between two of
/// these sets across a fetch is exactly the set of events worth replaying.
///
/// Other projects' batches are excluded too, and that is the point of the key
/// argument: one ledger holds many projects, every store fetches all of it, and
/// only this project's half is ours to replay. Without the filter the arrival of
/// an unrelated project's batch would read as "something new for me" and send
/// sync into a merge that has nothing to merge.
fn peer_batches(dir: &Path, own_uuid: &str, own_key: &str) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(out),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name == ".git" || name == own_uuid || !entry.path().is_dir() {
            continue;
        }
        for f in std::fs::read_dir(entry.path())? {
            let f = f?;
            let fname = f.file_name().to_string_lossy().to_string();
            if !fname.ends_with(".jsonl") {
                continue;
            }
            if crate::project::batch_key(&f.path()).as_deref() != Some(own_key) {
                continue;
            }
            out.insert(format!("{name}/{fname}"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ledger_clone_sits_beside_the_database() {
        let dir = ledger_dir(Path::new("/somewhere/.quipu/db.sqlite"));
        assert_eq!(dir, Path::new("/somewhere/.quipu/ledger"));
    }

    /// A header line as a batch's first line, enough for `project::batch_key`.
    fn header(uuid: &str, key: &str) -> String {
        format!(
            r#"{{"record":"header","format":"{}","project_uuid":"{uuid}","project_key":"{key}"}}"#,
            crate::cmd::export::FORMAT
        )
    }

    #[test]
    fn peer_batches_ignores_our_own_and_the_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for d in ["mine", "theirs", ".git"] {
            std::fs::create_dir(root.join(d)).unwrap();
            std::fs::write(root.join(d).join("a.jsonl"), header(d, "ours")).unwrap();
        }
        let seen = peer_batches(root, "mine", "ours").unwrap();
        assert_eq!(
            seen.into_iter().collect::<Vec<_>>(),
            vec!["theirs/a.jsonl".to_string()]
        );
    }

    #[test]
    fn peer_batches_ignores_another_project_on_the_same_ledger() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("peer")).unwrap();
        std::fs::write(root.join("peer").join("a.jsonl"), header("peer", "ours")).unwrap();
        std::fs::create_dir(root.join("stranger")).unwrap();
        std::fs::write(
            root.join("stranger").join("a.jsonl"),
            header("stranger", "theirs"),
        )
        .unwrap();

        let seen = peer_batches(root, "mine", "ours").unwrap();
        assert_eq!(
            seen.into_iter().collect::<Vec<_>>(),
            vec!["peer/a.jsonl".to_string()]
        );
    }

    #[test]
    fn a_keyless_batch_belongs_to_the_store_that_wrote_it_and_no_one_else() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("old")).unwrap();
        std::fs::write(
            root.join("old").join("a.jsonl"),
            format!(
                r#"{{"record":"header","format":"{}","project_uuid":"old"}}"#,
                crate::cmd::export::FORMAT
            ),
        )
        .unwrap();

        assert!(peer_batches(root, "mine", "ours").unwrap().is_empty());
        assert_eq!(
            peer_batches(root, "mine", &crate::project::legacy_key("old"))
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["old/a.jsonl".to_string()]
        );
    }
}
