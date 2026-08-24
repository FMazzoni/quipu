//! `qp init` — create a store, declaring whether it starts a project or joins one.
//!
#![doc = include_str!("../../docs/modules/init.md")]

use crate::outcome::{emit, Outcome};
use crate::{cmd::import, db, project};
use anyhow::Result;
use clap::Args;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Environment pointer at a ledger clone, for the guard to look at.
///
/// A brand-new store has no `meta` to read a remote out of, so the only ledger
/// `qp init` can see is one it is handed: `--from`, this, or a clone already
/// sitting beside the store.
const ENV_LEDGER: &str = "QUIPU_LEDGER";

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Display-id prefix (2–5 uppercase letters). Configurable at first init only. Default: QP.
    #[arg(long)]
    pub prefix: Option<String>,
    /// Tag auto-applied to every `qp add` in this store. Repeatable; additive across re-inits.
    #[arg(long = "default-tag", value_name = "NAME")]
    pub default_tag: Vec<String>,
    /// Join the project in this ledger directory as a new machine of it.
    #[arg(long, value_name = "LEDGER")]
    pub from: Option<PathBuf>,
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
    },
    /// Rebuild this store as the one the ledger came from.
    Restore { from: PathBuf },
}

pub fn run(db_path: &Path, a: InitArgs) -> Result<()> {
    let intent = decide(db_path, &a)?;

    let (conn, identity) = match &intent {
        Intent::Restore { from } => {
            // Init first so there is an empty store to restore into, then let
            // `import --restore` do the adopting. One code path for restore,
            // not a second copy of it living here — see `docs/modules/import.md`.
            let conn = db::init(db_path, None, &[], None)?;
            drop(conn);
            import::import(
                db_path,
                &import::ImportArgs {
                    from: from.clone(),
                    restore: true,
                    merge: false,
                    force: false,
                    json: false,
                },
            )?;
            (db::open(db_path)?, "restored")
        }
        Intent::Join {
            key,
            prefix,
            default_tags,
        } => {
            let mut tags = default_tags.clone();
            tags.extend(a.default_tag.iter().cloned());
            let p = a.prefix.as_deref().or(prefix.as_deref());
            let existed = existing_store_uuid(db_path).is_some();
            (
                db::init(db_path, p, &tags, Some(key))?,
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
        if a.restore {
            return Ok(Intent::Restore { from: from.clone() });
        }
        return join_from_ledger(from, a.project_key.as_deref());
    }
    if let Some(key) = &a.project_key {
        project::validate(key)?;
        return Ok(Intent::Join {
            key: key.clone(),
            prefix: None,
            default_tags: Vec::new(),
        });
    }
    if !a.new_project && existing_store_uuid(db_path).is_none() {
        refuse_if_a_ledger_holds_projects(db_path)?;
    }
    Ok(Intent::Start)
}

/// Picks the one project in a ledger directory, or makes the caller name it.
fn join_from_ledger(from: &Path, wanted: Option<&str>) -> Result<Intent> {
    if !from.is_dir() {
        return Err(db::invalid_input(format!(
            "`--from {}` is not a directory. Point it at a ledger clone, or at one \
             `<project_uuid>` batch directory inside one.",
            from.display()
        )));
    }
    let mut projects = project::scan(from)?;
    if let Some(want) = wanted {
        project::validate(want)?;
        projects.retain(|p| p.project_key == want);
        if projects.is_empty() {
            return Err(db::invalid_input(format!(
                "no project `{want}` in {}. Run without `--project-key` to see what is there.",
                from.display()
            )));
        }
    }
    match projects.len() {
        0 => Err(db::invalid_input(format!(
            "{} holds no quipu batches, so there is no project to join. `qp init` starts \
             one here instead.",
            from.display()
        ))),
        1 => {
            let p = projects.remove(0);
            Ok(Intent::Join {
                key: p.project_key,
                prefix: Some(p.display_prefix),
                default_tags: p.default_tags,
            })
        }
        n => Err(db::conflict(
            "ambiguous_project",
            format!(
                "{} holds {n} projects and joining picks exactly one. Name it with \
                 `--project-key <KEY>`:\n  {}",
                from.display(),
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

/// The failure this whole command exists to prevent.
///
/// Starting a new project on a populated shared ledger is legitimate — a ledger
/// is allowed to carry many — but it is almost never what someone means when
/// they type plain `qp init` next to one. Silently minting a key there produces
/// a store that syncs happily forever and whose tickets never reach the peers
/// it was meant to join, and nothing about that state looks wrong from inside.
/// So it is refused, and both intents are named.
fn refuse_if_a_ledger_holds_projects(db_path: &Path) -> Result<()> {
    for cand in visible_ledgers(db_path) {
        let Ok(projects) = project::scan(&cand) else {
            continue;
        };
        if projects.is_empty() {
            continue;
        }
        let names: Vec<String> = projects.iter().map(|p| p.project_key.clone()).collect();
        return Err(db::conflict(
            "ambiguous_project_intent",
            format!(
                "the ledger at {} already holds {} project(s):\n  {}\n\n\
                 Plain `qp init` would start a NEW project that merely shares that ledger. \
                 That is legal, and it is rarely what is meant — the tickets filed here would \
                 never reach the machines you were joining. Say which you want:\n\
                 \x20 qp init --from {}   join the project there\n\
                 \x20 qp init --new-project   start a new project on that ledger anyway",
                cand.display(),
                names.len(),
                names.join("\n  "),
                cand.display(),
            ),
            None,
        ));
    }
    Ok(())
}

/// Ledger directories `qp init` can see without a network round trip.
///
/// Only what it was handed. Sniffing the filesystem for something ledger-shaped
/// beside the store was tried and reverted: a directory named `ledger` next to a
/// database is not evidence of anything, and guessing from one refused fourteen
/// legitimate inits across the existing suites. A guard that fires on innocent
/// layouts is a guard people learn to pass a flag past.
///
/// The honest consequence: a ledger that exists only as a url nobody has cloned
/// is invisible here, and no amount of inference would change that. `--from`
/// and this variable are the two ways to tell init what it is standing next to.
fn visible_ledgers(_db_path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(v) = std::env::var_os(ENV_LEDGER) {
        let p = PathBuf::from(v);
        if p.is_dir() {
            out.push(p);
        }
    }
    out
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
