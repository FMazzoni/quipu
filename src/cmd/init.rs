//! `qp init` — create a store, declaring whether it starts a project or joins one.
//!
#![doc = include_str!("../../docs/modules/init.md")]

use crate::outcome::{emit, Outcome};
use crate::{cmd::import, cmd::sync, db, project, remote, s3};
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
    /// Join the project in this ledger — an `s3://` url or a directory of batches.
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
    /// Human name this store answers to. Lowercase letters, digits, hyphens.
    /// Defaults to the directory name; fixed once assigned.
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
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
    /// What this store answers to by name. Read back out of the store for the
    /// same reason `prefix` is: re-initialising keeps the name it already had.
    project_name: String,
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
        format!(
            "initialized at {} — {what} (name `{}`)",
            self.db_path, self.project_name
        )
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
        /// The batches to fold in, when the join read a ledger. `None` for a
        /// join by bare `--project-key`, which names a project without showing
        /// it.
        batches: Option<Materialised>,
    },
    /// Rebuild this store as the one the ledger came from.
    Restore {
        batches: Materialised,
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
    origin: Origin,
    url: Option<String>,
    label: String,
}

/// The two things `--from` may name, and nothing else.
///
/// A directory of batch files — what `qp export` writes, and what a restored
/// backup looks like — needs no network and is the disaster-recovery path. A
/// `s3://` url is the ledger proper. There is deliberately no git case: a fetch
/// takes a whole branch, so every store carried every other project's history,
/// and that is the cost this transport exists to remove.
enum Origin {
    Dir(PathBuf),
    Object(Box<s3::Client>),
}

/// Batch files on disk, ready to be read, plus the object keys they came from.
///
/// `_scratch` is the temp directory guard: dropping it deletes the download.
/// It is held rather than ignored so nothing this command materialises outlives
/// the command — the ledger has no local mirror, by design.
struct Materialised {
    dir: PathBuf,
    /// Object keys downloaded, so the first `qp sync` does not fetch them again.
    keys: Vec<String>,
    _scratch: Option<s3::Scratch>,
}

impl Source {
    /// The project keys this ledger holds.
    ///
    /// Both transports ask `project` the same question and get the same rule
    /// back — a project is a place holding at least one readable batch header,
    /// never a name in a listing. The object listing alone used to be the
    /// answer, and it counted a key prefix belonging to some other store as a
    /// project: `qp init --from` offered a candidate that `qp ledger` did not
    /// show, and joining it produced a store with nothing in it.
    fn project_keys(&self) -> Result<Vec<String>> {
        match &self.origin {
            Origin::Dir(d) => Ok(project::scan(d)?
                .into_iter()
                .map(|p| p.project_key)
                .collect()),
            Origin::Object(c) => project::scan_object(c),
        }
    }

    /// Brings one project's batches (or all of them) somewhere they can be read.
    fn materialise(&self, project: Option<&str>) -> Result<Materialised> {
        match &self.origin {
            Origin::Dir(d) => Ok(Materialised {
                dir: d.clone(),
                keys: Vec::new(),
                _scratch: None,
            }),
            Origin::Object(c) => {
                let scratch = s3::Scratch::new("join")?;
                let dir = scratch.path().join("ledger");
                std::fs::create_dir_all(&dir)?;
                let keys = sync::materialise(c, project, &dir)?;
                Ok(Materialised {
                    dir,
                    keys,
                    _scratch: Some(scratch),
                })
            }
        }
    }
}

pub fn run(db_path: &Path, a: InitArgs) -> Result<()> {
    // Before anything opens the store: `db::init` creates it, and by the time
    // the naming below runs there is no way left to tell a store this command
    // just made from one that was already here.
    let fresh = existing_store_uuid(db_path).is_none();
    let intent = decide(db_path, &a)?;

    let (conn, identity) = match &intent {
        Intent::Restore {
            batches,
            ledger_url,
        } => {
            // Init first so there is an empty store to restore into, then let
            // `import --restore` do the adopting. One code path for restore,
            // not a second copy of it living here — see `docs/modules/import.md`.
            let conn = db::init(db_path, None, &[], None)?;
            drop(conn);
            import::import(
                db_path,
                &import::ImportArgs {
                    from: batches.dir.clone(),
                    restore: true,
                    merge: false,
                    force: false,
                    json: false,
                },
            )?;
            let conn = db::open(db_path)?;
            attach_ledger(&conn, ledger_url.as_deref())?;
            sync::mark_seen(&conn, batches.keys.iter().map(String::as_str))?;
            (conn, "restored")
        }
        Intent::Join {
            key,
            prefix,
            default_tags,
            ledger_url,
            batches,
        } => {
            let mut tags = default_tags.clone();
            tags.extend(a.default_tag.iter().cloned());
            let p = a.prefix.as_deref().or(prefix.as_deref());
            let existed = existing_store_uuid(db_path).is_some();
            let conn = db::init(db_path, p, &tags, Some(key))?;
            attach_ledger(&conn, ledger_url.as_deref())?;
            drop(conn);
            if let Some(batches) = batches {
                fold_in(db_path, &batches.dir)?;
                // The keys this join already read. Without recording them the
                // first sync would download and re-merge the entire project —
                // idempotent, and a full re-fetch of history the store is
                // already holding.
                let conn = db::open(db_path)?;
                sync::mark_seen(&conn, batches.keys.iter().map(String::as_str))?;
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

    // After the store exists and before anything is reported: the name is not
    // part of `db::init`'s DDL bookkeeping, and every intent above ends with a
    // store that must have one.
    let project_name = project::assign_name(&conn, db_path, a.name.as_deref(), fresh)?;

    let outcome = InitOutcome {
        db_path: db_path.display().to_string(),
        prefix: db::display_prefix(&conn)?,
        project_name,
        schema_version: db::SCHEMA_VERSION.to_string(),
        project_uuid: crate::project::store_uuid(&conn)?,
        project_key: project::require(&conn)?,
        identity,
    };
    emit(a.json, &outcome)
}

/// Resolves the flags into one intent, or refuses to guess.
fn decide(db_path: &Path, a: &InitArgs) -> Result<Intent> {
    // Before anything is created. A name refused after `db::init` would leave a
    // store on disk that the failed command claims not to have made, carrying
    // the derived placeholder rather than the name that was asked for.
    if let Some(name) = &a.name {
        project::validate_name(name)?;
    }
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
        let source = resolve_source(from)?;
        if a.restore {
            // A restore adopts one store's identity, so it must be pointed at
            // one project. Naming it is only skippable when the ledger holds
            // exactly one.
            let key = a.project_key.clone();
            let key = match key {
                Some(k) => Some(k),
                None => pick_the_only_project(&source)?,
            };
            return Ok(Intent::Restore {
                batches: source.materialise(key.as_deref())?,
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
            batches: None,
        });
    }
    if !a.new_project && existing_store_uuid(db_path).is_none() {
        refuse_if_a_ledger_holds_projects()?;
    }
    Ok(Intent::Start)
}

/// Decides what `--from` names: a directory of batch files, or a ledger url.
///
/// Two cases and no guessing. A directory that exists is read in place, which is
/// the disaster-recovery path — an exported dump needs no credentials and no
/// network. Anything else must be an `s3://` url, and is refused with the url
/// form spelled out rather than being probed as some other transport.
fn resolve_source(from: &str) -> Result<Source> {
    if s3::is_object_url(from) {
        return Ok(Source {
            origin: Origin::Object(Box::new(s3::Client::open(from)?)),
            url: Some(from.to_string()),
            label: from.to_string(),
        });
    }
    let p = Path::new(from);
    if p.is_dir() {
        return Ok(Source {
            origin: Origin::Dir(p.to_path_buf()),
            url: None,
            label: p.display().to_string(),
        });
    }
    if p.exists() {
        return Err(db::invalid_input(format!(
            "`--from {from}` is a file. Point it at a ledger url \
             (`s3://<host>/<bucket>[/<prefix>]`), at a directory of exported batches, or \
             at one `<project_uuid>` batch directory inside one."
        )));
    }
    Err(db::invalid_input(format!(
        "`--from {from}`: not a directory, and not a ledger url. The ledger lives in \
         object storage and the url form is `s3://<host>/<bucket>[/<prefix>]`."
    )))
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
///
/// Enumerating comes before downloading, which is the shape the object store
/// made possible: the project keys are the first key segment, so choosing costs
/// one listing plus one batch read per candidate, and only the chosen project is
/// ever transferred. The git ledger had to fetch everything before it could tell
/// you what was in it.
fn join_from_ledger(source: &Source, wanted: Option<&str>) -> Result<Intent> {
    let mut keys = source.project_keys()?;
    if let Some(want) = wanted {
        project::validate(want)?;
        keys.retain(|k| k == want);
        if keys.is_empty() {
            return Err(db::invalid_input(format!(
                "no project `{want}` in {}. Run without `--project-key` to see what is there.",
                source.label
            )));
        }
    }
    match keys.len() {
        0 => Err(db::invalid_input(empty_ledger_note(source))),
        1 => {
            let key = keys.remove(0);
            let batches = source.materialise(Some(&key))?;
            // The envelope — display prefix, default tags — lives in the batch
            // headers, so it is read from what was just materialised rather than
            // from the listing. A project whose batches say nothing about it
            // keeps quipu's defaults instead of inventing some.
            let found = project::scan(&batches.dir)?
                .into_iter()
                .find(|p| p.project_key == key);
            let (prefix, default_tags) = match found {
                Some(p) => (Some(p.display_prefix), p.default_tags),
                None => (None, Vec::new()),
            };
            Ok(Intent::Join {
                key,
                prefix,
                default_tags,
                ledger_url: source.url.clone(),
                batches: Some(batches),
            })
        }
        n => Err(db::conflict(
            "ambiguous_project",
            format!(
                "{} holds {n} projects and joining picks exactly one. Name it with \
                 `--project-key <KEY>`:\n  {}",
                source.label,
                keys.join("\n  ")
            ),
            None,
        )),
    }
}

/// The single project a ledger holds, or a refusal naming the choice to make.
fn pick_the_only_project(source: &Source) -> Result<Option<String>> {
    let keys = source.project_keys()?;
    match keys.len() {
        0 => Ok(None),
        1 => Ok(Some(keys[0].clone())),
        n => Err(db::conflict(
            "ambiguous_project",
            format!(
                "{} holds {n} projects and `--restore` rebuilds this store as a machine of \
                 exactly one. Name it with `--project-key <KEY>`:\n  {}",
                source.label,
                keys.join("\n  ")
            ),
            None,
        )),
    }
}

/// What to say about a ledger that holds no batches.
///
/// An empty answer has two very different causes and they need different fixes,
/// so both are named: the bucket really is empty, or the credentials in this
/// environment cannot see into the prefix that holds it. A LIST scoped outside
/// your policy does not fail — it returns nothing, which is exactly what a
/// genuinely empty ledger returns.
fn empty_ledger_note(source: &Source) -> String {
    let mut s = format!(
        "{} holds no quipu batches, so there is no project to join.",
        source.label
    );
    if source.url.is_some() {
        s.push_str(&format!(
            "\n\nIf you expected one: a listing scoped outside your credentials' policy \
             comes back empty rather than refused, so an empty answer and an unreadable \
             prefix look identical. Check that {} and {} name a key allowed to read this \
             bucket and prefix.",
            s3::ENV_KEY,
            s3::ENV_SECRET
        ));
    } else {
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
fn refuse_if_a_ledger_holds_projects() -> Result<()> {
    let Some(source) = visible_ledger() else {
        return Ok(());
    };
    let Ok(names) = source.project_keys() else {
        return Ok(());
    };
    if names.is_empty() {
        return Ok(());
    }
    let label = source.label.clone();
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
fn visible_ledger() -> Option<Source> {
    let raw = std::env::var_os(ENV_LEDGER)?;
    let raw = raw.to_str()?.trim().to_string();
    if raw.is_empty() {
        return None;
    }
    // The same resolution `--from` uses, so the guard and the joining path can
    // never disagree about what a given ledger is or what is in it.
    resolve_source(&raw).ok()
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
