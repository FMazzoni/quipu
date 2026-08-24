//! `qp init` — create a store, declaring whether it starts a project or joins one.
//!
#![doc = include_str!("../../docs/modules/init.md")]

use crate::outcome::{emit, Outcome};
use crate::{cmd::import, cmd::sync, db, project, remote};
use anyhow::Result;
use clap::Args;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Environment pointer at the ledger, for the guard to look at.
///
/// A brand-new store has no `meta` to read a remote out of, so the only ledger
/// `qp init` can see is one it is handed: `--from`, or this. Either may be a
/// directory or a url.
const ENV_LEDGER: &str = "QUIPU_LEDGER";

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Display-id prefix (2–5 uppercase letters). Configurable at first init only. Default: QP.
    #[arg(long)]
    pub prefix: Option<String>,
    /// Tag auto-applied to every `qp add` in this store. Repeatable; additive across re-inits.
    #[arg(long = "default-tag", value_name = "NAME")]
    pub default_tag: Vec<String>,
    /// Join the project in this ledger — a git url or a local directory — as a new machine of it.
    #[arg(long, value_name = "LEDGER")]
    pub from: Option<String>,
    /// With `--from`: rebuild THIS store from the ledger, identity and all (disaster recovery).
    #[arg(long)]
    pub restore: bool,
    /// Join a project by key, without a ledger to read it from.
    #[arg(long, value_name = "KEY")]
    pub project_key: Option<String>,
    /// Start a NEW project even where a ledger already holds others.
    #[arg(long)]
    pub new_project: bool,
    /// Emit JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

/// Success output for `qp init`.
///
/// `prefix` is read back out of the store rather than echoing `--prefix`,
/// because re-initializing an existing store keeps the original prefix and
/// ignores the flag — echoing the argument would report a rename that did not
/// happen. `project_key` and `identity` are the pair that makes the difference
/// between starting and joining observable at the moment it is decided.
#[derive(Serialize)]
pub struct InitOutcome {
    db_path: String,
    prefix: String,
    schema_version: String,
    project_uuid: String,
    project_key: String,
    /// `minted` (new project), `inherited` (joined one), `restored`, or
    /// `existing` (this store already had one).
    identity: &'static str,
}

impl Outcome for InitOutcome {
    fn human(&self) -> String {
        let what = match self.identity {
            "inherited" => format!("joined project {}", self.project_key),
            "restored" => format!("restored into project {}", self.project_key),
            "minted" => format!("started project {}", self.project_key),
            _ => format!("project {}", self.project_key),
        };
        format!("initialized at {} — {what}", self.db_path)
    }
}

/// What this invocation was asked to do with identity. Never inferred.
enum Intent {
    /// Mint a new project identity.
    Start,
    /// Adopt an existing one, with whatever envelope came with it.
    Join {
        key: String,
        prefix: Option<String>,
        default_tags: Vec<String>,
        ledger_url: Option<String>,
        /// The ledger to fold in, when the join read one. `None` for a join by
        /// bare `--project-key`, which names a project without showing it.
        dir: Option<PathBuf>,
    },
    /// Rebuild this store as the one the ledger came from.
    Restore {
        dir: PathBuf,
        ledger_url: Option<String>,
    },
}

/// A ledger this command can read, and how it was named.
///
/// `url` is `Some` when the ledger was named by url rather than by path, which
/// is the case that has to end with `remote.ledger` set: someone joining from a
/// url has just told quipu where the ledger is, and making them repeat it to
/// `qp remote set` before their first sync is the ceremony this path exists to
/// remove.
struct Source {
    dir: PathBuf,
    url: Option<String>,
}

pub fn run(db_path: &Path, a: InitArgs) -> Result<()> {
    let intent = decide(db_path, &a)?;

    let (conn, identity) = match &intent {
        Intent::Restore { dir, ledger_url } => {
            // Init first so there is an empty store to restore into, then let
            // `import --restore` do the adopting. One code path for restore,
            // not a second copy of it living here — see `docs/modules/import.md`.
            let conn = db::init(db_path, None, &[], None)?;
            drop(conn);
            import::import(
                db_path,
                &import::ImportArgs {
                    from: dir.clone(),
                    restore: true,
                    merge: false,
                    force: false,
                    json: false,
                },
            )?;
            let conn = db::open(db_path)?;
            attach_ledger(&conn, ledger_url.as_deref())?;
            (conn, "restored")
        }
        Intent::Join {
            key,
            prefix,
            default_tags,
            ledger_url,
            dir,
        } => {
            let mut tags = default_tags.clone();
            tags.extend(a.default_tag.iter().cloned());
            let p = a.prefix.as_deref().or(prefix.as_deref());
            let existed = existing_store_uuid(db_path).is_some();
            let conn = db::init(db_path, p, &tags, Some(key))?;
            attach_ledger(&conn, ledger_url.as_deref())?;
            drop(conn);
            if let Some(dir) = dir {
                fold_in(db_path, dir)?;
            }
            (
                db::open(db_path)?,
                if existed { "existing" } else { "inherited" },
            )
        }
        Intent::Start => {
            let existed = existing_store_uuid(db_path).is_some();
            (
                db::init(db_path, a.prefix.as_deref(), &a.default_tag, None)?,
                if existed { "existing" } else { "minted" },
            )
        }
    };

    let outcome = InitOutcome {
        db_path: db_path.display().to_string(),
        prefix: db::display_prefix(&conn)?,
        schema_version: db::SCHEMA_VERSION.to_string(),
        project_uuid: crate::claimref::project_uuid(&conn)?,
        project_key: project::require(&conn)?,
        identity,
    };
    emit(a.json, &outcome)
}

/// Resolves the flags into one intent, or refuses to guess.
fn decide(db_path: &Path, a: &InitArgs) -> Result<Intent> {
    if a.new_project && (a.from.is_some() || a.project_key.is_some()) {
        return Err(db::invalid_input(
            "`--new-project` starts a project and `--from`/`--project-key` join one; \
             pass exactly one of them.",
        ));
    }
    if a.restore && a.from.is_none() {
        return Err(db::invalid_input(
            "`--restore` rebuilds this store from a ledger and needs `--from <LEDGER>`.",
        ));
    }
    if let Some(from) = &a.from {
        if a.prefix.is_some() {
            return Err(db::invalid_input(
                "`--prefix` cannot be combined with `--from`: the display prefix comes from \
                 the project being joined, and two machines of one project that mint \
                 different prefixes name the same ticket two ways.",
            ));
        }
        let source = resolve_source(db_path, from)?;
        if a.restore {
            return Ok(Intent::Restore {
                dir: source.dir,
                ledger_url: source.url,
            });
        }
        return join_from_ledger(&source, a.project_key.as_deref());
    }
    if let Some(key) = &a.project_key {
        project::validate(key)?;
        return Ok(Intent::Join {
            key: key.clone(),
            prefix: None,
            default_tags: Vec::new(),
            ledger_url: None,
            dir: None,
        });
    }
    if !a.new_project && existing_store_uuid(db_path).is_none() {
        refuse_if_a_ledger_holds_projects(db_path)?;
    }
    Ok(Intent::Start)
}

/// Brings the ledger named by `--from` onto this machine, url or directory.
///
/// Someone setting up a new machine has a url and nothing else — that is the
/// whole situation this path exists for — so requiring a `git clone` by hand
/// first would leave the setup story as undefined as it was.
///
/// The order of the questions is the load-bearing part, and the obvious order is
/// wrong. "Is it a directory?" cannot come first, because the most ordinary
/// local ledger — a bare repository — is BOTH a directory and a git url, and
/// reading it as a directory finds nothing: its batches are in `objects`, not in
/// a tree. So the question asked first is the one that is actually about
/// content — *does this directory hold batches* — and only a directory that does
/// is read in place. Anything else that is a repository is fetched from.
fn resolve_source(db_path: &Path, from: &str) -> Result<Source> {
    let p = Path::new(from);
    if p.is_dir() {
        let holds_batches = project::scan(p).map(|v| !v.is_empty()).unwrap_or(false);
        if holds_batches || !is_git_repo(p) {
            return Ok(Source {
                dir: p.to_path_buf(),
                url: None,
            });
        }
        // A repository with no batches in its tree: a bare ledger, or a clone
        // sitting on the wrong branch. Both are answered by fetching.
        let dir = sync::materialise(db_path, from)?;
        return Ok(Source {
            dir,
            url: Some(from.to_string()),
        });
    }
    if p.exists() {
        return Err(db::invalid_input(format!(
            "`--from {from}` is a file. Point it at a ledger url, at a ledger clone, or \
             at one `<project_uuid>` batch directory inside one."
        )));
    }
    if from.starts_with('/') || from.starts_with('.') || from.starts_with('~') {
        return Err(db::invalid_input(format!(
            "`--from {from}`: no such directory. A path that does not exist is not read as \
             a url — pass the ledger's url itself if that is what you meant."
        )));
    }
    remote::validate_url(from)?;
    let dir = sync::materialise(db_path, from)?;
    Ok(Source {
        dir,
        url: Some(from.to_string()),
    })
}

/// Whether a directory is a git repository, bare or not.
fn is_git_repo(dir: &Path) -> bool {
    dir.join(".git").exists() || (dir.join("HEAD").is_file() && dir.join("objects").is_dir())
}

/// Folds the joined project's history into the new store.
///
/// A join must not leave the tickets behind, and it cannot leave them for the
/// first `qp sync` either — measured, not assumed. Sync decides what is new by
/// diffing the batch files in its clone across a fetch, so batches already
/// sitting there when it starts are "already seen" forever, and a store joined
/// from a url silently never receives a single ticket its peers wrote before it
/// arrived. Its own board looks fine, which is what makes it expensive.
///
/// So the join folds them in itself, through the same `--merge` a sync uses. It
/// also makes a join from a url and a join from a directory produce identical
/// stores, rather than two shapes depending on where the clone happened to sit.
///
/// The merge order and the legality guard are `import`'s, untouched: a ledger
/// that would refuse a merge into a live store refuses here too, with its own
/// diagnosis, and the store is left holding the identity it just established.
fn fold_in(db_path: &Path, dir: &Path) -> Result<()> {
    import::import(
        db_path,
        &import::ImportArgs {
            from: dir.to_path_buf(),
            restore: false,
            merge: true,
            force: false,
            json: false,
        },
    )
    .map(|_| ())
}

/// Records the ledger a join came from, so the new machine can sync at once.
fn attach_ledger(conn: &rusqlite::Connection, url: Option<&str>) -> Result<()> {
    let Some(url) = url else { return Ok(()) };
    remote::set(conn, remote::KEY_LEDGER, url)
}

/// Picks the one project in a ledger, or makes the caller name it.
fn join_from_ledger(source: &Source, wanted: Option<&str>) -> Result<Intent> {
    let mut projects = project::scan(&source.dir)?;
    if let Some(want) = wanted {
        project::validate(want)?;
        projects.retain(|p| p.project_key == want);
        if projects.is_empty() {
            return Err(db::invalid_input(format!(
                "no project `{want}` in {}. Run without `--project-key` to see what is there.",
                source.label()
            )));
        }
    }
    match projects.len() {
        0 => Err(db::invalid_input(empty_ledger_note(source))),
        1 => {
            let p = projects.remove(0);
            Ok(Intent::Join {
                key: p.project_key,
                prefix: Some(p.display_prefix),
                default_tags: p.default_tags,
                ledger_url: source.url.clone(),
                dir: Some(source.dir.clone()),
            })
        }
        n => Err(db::conflict(
            "ambiguous_project",
            format!(
                "{} holds {n} projects and joining picks exactly one. Name it with \
                 `--project-key <KEY>`:\n  {}",
                source.label(),
                projects
                    .iter()
                    .map(|p| format!("{} ({})", p.project_key, p.display_prefix))
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ),
            None,
        )),
    }
}

/// What to say about a ledger directory holding no batches.
///
/// The honest cause is usually not "empty ledger". Batches live on
/// `quipu-ledger`, which is not a bare repository's default `HEAD`, so
/// `git clone <ledger>` by hand lands on a branch that does not exist and
/// produces an empty working tree — for a ledger that is completely full. A
/// message that only says "no project here" sends someone looking for a missing
/// ledger instead of a missing branch, so it says both.
fn empty_ledger_note(source: &Source) -> String {
    let branch = sync::LEDGER_BRANCH;
    let mut s = format!(
        "{} holds no quipu batches, so there is no project to join.\n\n\
         If you expected one: batches live on the `{branch}` branch, which is not a \
         bare repository's default HEAD, so a plain `git clone` of a ledger checks out \
         an empty tree and looks exactly like this. Check with \
         `git -C <clone> log --oneline {branch}`, or pass `--from <url>` and let quipu \
         fetch the branch itself.",
        source.label()
    );
    if source.url.is_none() {
        s.push_str("\n\nIf the ledger really is empty, `qp init` starts a project here instead.");
    }
    s
}

/// The failure this whole command exists to prevent.
///
/// Starting a new project on a populated shared ledger is legitimate — a ledger
/// is allowed to carry many — but it is almost never what someone means when
/// they type plain `qp init` next to one. Silently minting a key there produces
/// a store that syncs happily forever and whose tickets never reach the peers
/// it was meant to join, and nothing about that state looks wrong from inside.
/// So it is refused, and both intents are named.
fn refuse_if_a_ledger_holds_projects(db_path: &Path) -> Result<()> {
    let Some(source) = visible_ledger(db_path) else {
        return Ok(());
    };
    let Ok(projects) = project::scan(&source.dir) else {
        return Ok(());
    };
    if projects.is_empty() {
        return Ok(());
    }
    let names: Vec<String> = projects.iter().map(|p| p.project_key.clone()).collect();
    let label = source.label();
    Err(db::conflict(
        "ambiguous_project_intent",
        format!(
            "the ledger at {label} already holds {} project(s):\n  {}\n\n\
             Plain `qp init` would start a NEW project that merely shares that ledger. \
             That is legal, and it is rarely what is meant — the tickets filed here would \
             never reach the machines you were joining. Say which you want:\n\
             \x20 qp init --from {label}   join the project there\n\
             \x20 qp init --new-project   start a new project on that ledger anyway",
            names.len(),
            names.join("\n  "),
        ),
        None,
    ))
}

/// The ledger `qp init` can see, and how to name it back to the person.
///
/// Only what it was handed: `$QUIPU_LEDGER`, as a directory or a url. Sniffing
/// the filesystem for something ledger-shaped beside the store was tried and
/// reverted — a directory named `ledger` next to a database is not evidence of
/// anything, and guessing from one refused fourteen legitimate inits across the
/// existing suites. A guard that fires on innocent layouts is a guard people
/// learn to pass a flag past.
///
/// A url is fetched, which is the one place `qp init` touches the network, and
/// only when that variable is set. An unreachable ledger yields `None` rather
/// than an error: someone on a plane initialising a store must not be blocked by
/// a guard that cannot see what it is guarding.
fn visible_ledger(db_path: &Path) -> Option<Source> {
    let raw = std::env::var_os(ENV_LEDGER)?;
    let raw = raw.to_str()?.trim().to_string();
    if raw.is_empty() {
        return None;
    }
    // The same resolution `--from` uses, so the guard and the joining path can
    // never disagree about what a given ledger is or what is in it.
    resolve_source(db_path, &raw).ok()
}

impl Source {
    /// How to name this ledger back to the person.
    ///
    /// The url when there was one: that is what they typed, and what they can
    /// type back. The clone path it landed in is quipu's bookkeeping.
    fn label(&self) -> String {
        match &self.url {
            Some(u) => u.clone(),
            None => self.dir.display().to_string(),
        }
    }
}

/// The store's uuid if a store is already here, without creating or migrating anything.
///
/// Deliberately not `db::open`: opening would create the very store whose
/// absence is the question, and every error here — no file, no `meta`, no row —
/// means the same thing.
fn existing_store_uuid(db_path: &Path) -> Option<String> {
    if !db_path.exists() {
        return None;
    }
    let conn = rusqlite::Connection::open(db_path).ok()?;
    conn.query_row(
        "SELECT value FROM meta WHERE key = 'project_uuid'",
        [],
        |r| r.get::<_, String>(0),
    )
    .ok()
}
