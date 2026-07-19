//! `qp` — quipu CLI entry point. Parses subcommands and dispatches to `src/cmd/<name>.rs`.
//! Exit codes: 0 success | 1 generic error | 2 constraint violation | 3 wait timeout |
//! 4 wait --cohort-done matched an empty cohort.
//!
//! Architecture overview — state machine, guarded-transition contract, module
//! map — lives in `docs/architecture.md`, included below rather than inlined so
//! that reading this file costs one line instead of the whole document.
//!
#![doc = include_str!("../docs/architecture.md")]

mod cmd;
mod db;
mod id;
mod outcome;
mod store;
mod time;

use clap::{Parser, Subcommand};
use outcome::Outcome;
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "quipu",
    bin_name = "qp",
    version,
    about = "Structured task substrate for agent orchestration",
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[arg(long, global = true, env = "QP_DB")]
    db: Option<std::path::PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Initialize a store in the current directory
    Init {
        /// Display-id prefix (2–5 uppercase letters). Configurable at first init only. Default: QP.
        #[arg(long)]
        prefix: Option<String>,
        /// Tag auto-applied to every `qp add` in this store. Repeatable; additive across re-inits.
        #[arg(long = "default-tag", value_name = "NAME")]
        default_tag: Vec<String>,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Add a new task
    Add(cmd::add::AddArgs),
    /// Assign a ready task to an agent (orchestrator only)
    Assign(cmd::assign::AssignArgs),
    /// Claim an assigned task as the running agent
    Claim(cmd::claim::ClaimArgs),
    /// Complete a running task (records decisions/artifacts)
    Complete(cmd::complete::CompleteArgs),
    /// Mark a running task blocked with a reason
    Block(cmd::block::BlockArgs),
    /// Cancel a task (any non-terminal state)
    Cancel(cmd::cancel::CancelArgs),
    /// Agent self-release of a claim (running → ready)
    Abandon(cmd::abandon::AbandonArgs),
    /// Orchestrator force-release of a claim
    Reclaim(cmd::reclaim::ReclaimArgs),
    /// Log a free-form event against a task
    Log(cmd::log::LogArgs),
    /// Manage tags on a task
    Tag(cmd::tag::TagArgs),
    /// Manage relations between tasks
    Relation(cmd::relation::RelationArgs),
    /// Render the task DAG
    Tree(cmd::tree::TreeArgs),
    /// Show the event timeline (per-task or global)
    Timeline(cmd::timeline::TimelineArgs),
    /// Show the current wave: ready / assigned / running / pending groups
    Wave(cmd::wave::WaveArgs),
    /// Snapshot of state counts
    Status(cmd::status::StatusArgs),
    /// List tasks with filters
    List(cmd::list::ListArgs),
    /// Decision events (filter alias over timeline)
    Decisions(cmd::decisions::DecisionsArgs),
    /// Block until a tag/state cohort drains
    Wait(cmd::wait::WaitArgs),
    /// Tail new events as they are recorded
    Watch(cmd::watch::WatchArgs),
    /// Install bundled skills into Claude Code's skill dir
    InstallSkills(cmd::install_skills::InstallSkillsArgs),
    /// Add or remove a dep edge between tasks
    Depends(cmd::depends::DependsArgs),
    /// Mutate task fields (title, tier, description)
    Edit(cmd::edit::EditArgs),
    /// Emit a structured JSON snapshot of the store
    Report(cmd::report::ReportArgs),
    /// Show a single-ticket detail view (human or JSON)
    Show(cmd::show::ShowArgs),
}

#[derive(Serialize)]
struct InitOutcome {
    db_path: String,
    prefix: String,
    schema_version: String,
}
impl Outcome for InitOutcome {
    fn human(&self) -> String {
        format!("initialized at {}", self.db_path)
    }
}

/// Whether `--json` was passed for this invocation.
///
/// `real_main()` returns `anyhow::Result<()>`, so by the time an error
/// reaches `main()` the parsed args (and their `--json` flag) are gone. Each
/// mutating command owns its own `--json` field on its `*Args` struct (no
/// global clap flag — `show`/`report` already define their own local
/// `--json`, and a parent-level global of the same name would collide with
/// those). So: extract the flag from the *parsed* `Cmd` up front, before
/// dispatch, and thread it separately into both the success path (per
/// command, via `outcome::emit`) and the error path (here, in `main`).
fn wants_json(cmd: &Cmd) -> bool {
    match cmd {
        Cmd::Init { json, .. } => *json,
        Cmd::Add(a) => a.json,
        Cmd::Assign(a) => a.json,
        Cmd::Claim(a) => a.json,
        Cmd::Complete(a) => a.json,
        Cmd::Block(a) => a.json,
        Cmd::Cancel(a) => a.json,
        Cmd::Abandon(a) => a.json,
        Cmd::Reclaim(a) => a.json,
        Cmd::Log(a) => a.json,
        Cmd::Tag(a) => a.json,
        Cmd::Depends(a) => a.json,
        Cmd::Edit(a) => a.json,
        Cmd::Show(a) => a.json,
        Cmd::Report(a) => a.json,
        Cmd::Relation(a) => a.json(),
        Cmd::Tree(_)
        | Cmd::Timeline(_)
        | Cmd::Wave(_)
        | Cmd::Status(_)
        | Cmd::List(_)
        | Cmd::Decisions(_)
        | Cmd::Wait(_)
        | Cmd::Watch(_)
        | Cmd::InstallSkills(_) => false,
    }
}

fn main() {
    let cli = Cli::parse();
    let json = wants_json(&cli.cmd);
    if let Err(e) = real_main(cli, json) {
        if json {
            let body = if let Some(err) = e.downcast_ref::<db::QuipuError>() {
                err.to_json()
            } else {
                serde_json::json!({"kind": "internal", "message": format!("{e:#}")})
            };
            eprintln!("{}", serde_json::json!({"error": body}));
        } else {
            eprintln!("error: {e:#}");
        }
        let code = if let Some(err) = e.downcast_ref::<db::QuipuError>() {
            err.exit_code()
        } else {
            1
        };
        std::process::exit(code);
    }
}

fn real_main(cli: Cli, json: bool) -> anyhow::Result<()> {
    let db_path = db::resolve_path(cli.db.clone())?;
    // `json` is threaded in so the warning matches the stream's format: under
    // --json stderr is JSON Lines, so a prose warning here would make the
    // error envelope on the following line unparseable. See QP-120.
    db::warn_on_project_mismatch(&cli.db, json)?;
    match cli.cmd {
        Cmd::Init {
            prefix,
            default_tag,
            json,
        } => {
            let conn = db::init(&db_path, prefix.as_deref(), &default_tag)?;
            let prefix = db::display_prefix(&conn)?;
            let outcome = InitOutcome {
                db_path: db_path.display().to_string(),
                prefix,
                schema_version: db::SCHEMA_VERSION.to_string(),
            };
            outcome::emit(json, &outcome)
        }
        Cmd::Add(a) => cmd::add::run(&db_path, a),
        Cmd::Assign(a) => cmd::assign::run(&db_path, a),
        Cmd::Claim(a) => cmd::claim::run(&db_path, a),
        Cmd::Complete(a) => cmd::complete::run(&db_path, a),
        Cmd::Block(a) => cmd::block::run(&db_path, a),
        Cmd::Cancel(a) => cmd::cancel::run(&db_path, a),
        Cmd::Abandon(a) => cmd::abandon::run(&db_path, a),
        Cmd::Reclaim(a) => cmd::reclaim::run(&db_path, a),
        Cmd::Log(a) => cmd::log::run(&db_path, a),
        Cmd::Tag(a) => cmd::tag::run(&db_path, a),
        Cmd::Relation(a) => cmd::relation::run(&db_path, a),
        Cmd::Timeline(a) => cmd::timeline::run(&db_path, a),
        Cmd::Decisions(a) => cmd::decisions::run(&db_path, a),
        Cmd::Tree(a) => cmd::tree::run(&db_path, a),
        Cmd::Status(a) => cmd::status::run(&db_path, a),
        Cmd::List(a) => cmd::list::run(&db_path, a),
        Cmd::Wave(a) => cmd::wave::run(&db_path, a),
        Cmd::Wait(a) => cmd::wait::run(&db_path, a),
        Cmd::Watch(a) => cmd::watch::run(&db_path, a),
        Cmd::InstallSkills(a) => cmd::install_skills::run(a),
        Cmd::Depends(a) => cmd::depends::run(&db_path, a),
        Cmd::Edit(a) => cmd::edit::run(&db_path, a),
        Cmd::Report(a) => cmd::report::run(&db_path, a),
        Cmd::Show(a) => cmd::show::run(&db_path, a),
        #[allow(unreachable_patterns)]
        _ => {
            eprintln!("not implemented yet");
            std::process::exit(1);
        }
    }
}
