//! `qp ledger` — the read-only view of every project the shared ledger holds.
//!
#![doc = include_str!("../../docs/modules/ledger.md")]

use crate::outcome::{emit, Outcome};
use crate::{db, project, remote, store};
use anyhow::{Context, Result};
use clap::Args;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct LedgerArgs {
    /// Only tasks in this state.
    #[arg(long = "state")]
    pub state: Option<db::State>,
    /// Glob pattern (e.g. `kind:bug`); repeatable, all must match.
    #[arg(long)]
    pub tag: Vec<String>,
    /// Glob pattern over the last agent assigned (e.g. `claude-code:*`).
    #[arg(long = "assigned-to")]
    pub assigned_to: Option<String>,
    /// Emit JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

/// One task as the ledger describes it, named by the store that minted it.
#[derive(Serialize)]
struct TaskView {
    /// `<prefix>-<origin id>`, disambiguated when one project holds two stores
    /// that independently minted the same number.
    display_id: String,
    project_key: String,
    origin_uuid: String,
    origin_task_id: i64,
    title: String,
    state: String,
    tier: Option<String>,
    agent: Option<String>,
    tags: Vec<String>,
}

/// One project's slice of the ledger.
#[derive(Serialize)]
struct ProjectView {
    project_key: String,
    display_prefix: String,
    /// Whether this is the project the store the command ran against belongs to.
    is_local: bool,
    tasks: Vec<TaskView>,
}

/// The whole answer, with its provenance attached to it rather than implied.
#[derive(Serialize)]
struct LedgerOutcome {
    /// Always `"ledger"`. A consumer that flattens the projects still carries
    /// the one fact that must not be lost: these rows are not local state.
    source: &'static str,
    ledger: String,
    local_project_key: Option<String>,
    projects: Vec<ProjectView>,
}

impl Outcome for LedgerOutcome {
    fn human(&self) -> String {
        let mut s = format!(
            "ledger {} — {} project(s), read-only\n\
             (from the ledger as of this fetch; this store was not written to, and \
             anything not yet synced is not here)",
            self.ledger,
            self.projects.len()
        );
        for p in &self.projects {
            s.push_str(&format!(
                "\n\nproject {}  prefix {}{}\nID\tSTATE\tAGENT\tTAGS\tTITLE",
                p.project_key,
                p.display_prefix,
                if p.is_local { "  (this store)" } else { "" }
            ));
            for t in &p.tasks {
                s.push_str(&format!(
                    "\n{}\t{}\t{}\t{}\t{}",
                    t.display_id,
                    t.state,
                    t.agent.as_deref().unwrap_or("-"),
                    if t.tags.is_empty() {
                        "-".to_string()
                    } else {
                        t.tags.join(",")
                    },
                    t.title
                ));
            }
            if p.tasks.is_empty() {
                s.push_str("\n(no tasks)");
            }
        }
        s
    }
}

pub fn run(db_path: &Path, a: LedgerArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let url = remote::load(&conn)?.ledger.ok_or_else(|| {
        db::invalid_input(
            "this store has no ledger remote, so there is no shared ledger to read; \
             run `qp remote set --ledger <url>` first. `qp ledger` never guesses a \
             source — a global view assembled from a guess is worse than no view.",
        )
    })?;
    let local_key = project::read(&conn)?;
    drop(conn);

    // The same ensure_repo / fetch / integrate `qp sync` runs, deliberately: a
    // second fetch path would be a second set of refspecs to get wrong, and the
    // refspec is what keeps `refs/quipu/claims/*` out of reach. It never pushes.
    let dir = crate::cmd::sync::materialise(db_path, &url)?;

    let scanned = project::scan(&dir)?;
    let prefixes: HashMap<&str, &str> = scanned
        .iter()
        .map(|p| (p.project_key.as_str(), p.display_prefix.as_str()))
        .collect();
    let grouped = project::batches_by_project(&dir)?;

    let scratch = Scratch::new()?;
    let filter = store::TaskFilter {
        state: a.state.map(|s| s.as_str()),
        assigned_to_glob: a.assigned_to.as_deref(),
        tag_globs: &a.tag,
        tier: None,
    };

    let mut projects = Vec::new();
    for (n, (key, files)) in grouped.iter().enumerate() {
        let prefix = prefixes.get(key.as_str()).copied().unwrap_or("QP");
        let tasks = replay_project(&scratch, n, files, key, prefix, &filter)?;
        projects.push(ProjectView {
            project_key: key.clone(),
            display_prefix: prefix.to_string(),
            is_local: local_key.as_deref() == Some(key.as_str()),
            tasks,
        });
    }
    // The reader's own project first; it is the one they can act on.
    projects.sort_by_key(|p| !p.is_local);

    emit(
        a.json,
        &LedgerOutcome {
            source: "ledger",
            ledger: url,
            local_project_key: local_key,
            projects,
        },
    )
}

/// Replays one project's batches into a throwaway store and reads it back.
///
/// The projection is built by the ordinary merge path — `import::import` with
/// `--merge`, over the same `merge::sort_for_replay` order and the same legality
/// guard a real sync uses. A second replay written for this command would be a
/// second answer to "what does this log mean", and the one that drifts is the
/// one nobody is testing convergence against.
///
/// `--force` is set because the target is a temp file that is deleted moments
/// later: the guard exists to protect live data from an unjustifiable history,
/// and there is no live data here. Refusing instead would let one project's
/// damaged history blank out every other project in the view.
///
/// The throwaway is created AS A MACHINE OF THE PROJECT BEING REPLAYED, not as
/// a blank store. A merge scopes itself to the target store's own project key —
/// that is what keeps one ledger's projects from absorbing each other — so a
/// scratch store carrying a freshly minted key of its own would match none of
/// the batches staged for it, and the import would succeed having read nothing.
/// Stating the key is what makes the scope the project this call is about rather
/// than an accident of whatever database the replay happens to land in.
///
/// The parity check afterwards is the tripwire for that whole failure family: it
/// is the difference between an empty project and an empty projection, which
/// nothing downstream can tell apart — both render as a project with no tickets,
/// exit 0, and look exactly like a ledger nobody has written to.
fn replay_project(
    scratch: &Scratch,
    n: usize,
    files: &[PathBuf],
    project_key: &str,
    prefix: &str,
    filter: &store::TaskFilter,
) -> Result<Vec<TaskView>> {
    let root = scratch.0.join(n.to_string());
    let batches = root.join("batches");
    for f in files {
        // Under `<origin uuid>/<original name>`: a batch is named by the range
        // it closes over and the importer checks the name against the header,
        // so the copy has to keep both the name and one directory level.
        let dir = batches.join(
            f.parent()
                .and_then(Path::file_name)
                .unwrap_or_else(|| std::ffi::OsStr::new("batches")),
        );
        std::fs::create_dir_all(&dir)?;
        let name = f.file_name().context("batch file has no name")?;
        std::fs::copy(f, dir.join(name))
            .with_context(|| format!("staging {} for replay", f.display()))?;
    }

    let db_path = root.join("projection.sqlite");
    db::init(&db_path, None, &[], Some(project_key))?;
    let out = crate::cmd::import::import(
        &db_path,
        &crate::cmd::import::ImportArgs {
            from: batches,
            restore: false,
            merge: true,
            force: true,
            json: false,
        },
    )?;
    if out.batches != files.len() || out.foreign_batches != 0 {
        return Err(db::invariant(
            "ledger_scope",
            format!(
                "replaying project `{project_key}` staged {} batch(es) and the merge read \
                 {} of them, skipping {} as another project's. Every staged batch belongs \
                 to this project by construction, so a skipped one means the replay was \
                 scoped to something other than the project it is about — which would \
                 report this project as empty rather than fail.",
                files.len(),
                out.batches,
                out.foreign_batches
            ),
        ));
    }

    let conn = db::open(&db_path)?;
    let rows = store::tasks(&conn, filter)?;
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let mut tags_by = store::tags_by_task(&conn, &ids)?;
    let origins = origins(&conn)?;

    // Every task here is foreign to the throwaway store, so its local rowid is a
    // number this command minted and nobody has ever seen. The name that means
    // something is the canonical one — the store that created the task and the
    // rowid it holds THERE, which is the display id that project's own board
    // shows. Rendering the local one would invent ids for the whole ledger.
    let mut named: Vec<(String, String, i64, store::TaskRow)> = Vec::new();
    for row in rows {
        let (uuid, origin_id) = match origins.get(&row.id) {
            Some(o) => o.clone(),
            None => continue,
        };
        named.push((format!("{prefix}-{origin_id}"), uuid, origin_id, row));
    }
    // Two stores in one project can each mint a rowid 5 without ever having
    // met, and both are `QP-5`. Rare, and silently wrong if left alone.
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for (display, ..) in &named {
        *seen.entry(display.as_str()).or_default() += 1;
    }
    let dupes: Vec<String> = seen
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|(d, _)| (*d).to_string())
        .collect();

    let mut out = Vec::with_capacity(named.len());
    for (display, uuid, origin_id, row) in named {
        let display = if dupes.contains(&display) {
            format!("{display}@{}", short(&uuid))
        } else {
            display
        };
        out.push(TaskView {
            display_id: display,
            project_key: project_key.to_string(),
            origin_uuid: uuid,
            origin_task_id: origin_id,
            title: row.title,
            state: row.state,
            tier: row.tier,
            agent: row.agent,
            tags: tags_by.remove(&row.id).unwrap_or_default(),
        });
    }
    out.sort_by(|a, b| (&a.origin_uuid, a.origin_task_id).cmp(&(&b.origin_uuid, b.origin_task_id)));
    Ok(out)
}

/// The canonical `(store, rowid)` behind every task in a throwaway projection.
fn origins(conn: &Connection) -> Result<HashMap<i64, (String, i64)>> {
    let mut stmt = conn.prepare("SELECT task_id, origin_uuid, origin_task_id FROM task_origin")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            (r.get::<_, String>(1)?, r.get::<_, i64>(2)?),
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, origin) = row?;
        out.insert(id, origin);
    }
    Ok(out)
}

/// Enough of a uuid to tell two stores apart in a terminal.
fn short(uuid: &str) -> String {
    uuid.chars().take(8).collect()
}

/// A temp directory that removes itself, projections and staged batches and all.
///
/// The whole point of this command is that answering it writes nothing anyone
/// keeps. It lives under the system temp directory rather than beside the store
/// so a crash cannot leave a half-built projection inside `.quipu/`, where the
/// next reader would find something that looks like state.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self> {
        let nonce = crate::time::now_rfc3339().replace([':', '.', '-'], "");
        let dir = std::env::temp_dir().join(format!("quipu-ledger-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating scratch directory {}", dir.display()))?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
