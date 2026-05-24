//! `qp` — quipu CLI entry point. Parses subcommands and dispatches to `src/cmd/<name>.rs`.
//! Exit codes: 0 success | 1 generic error | 2 constraint violation | 3 wait timeout.

mod cmd;
mod db;
mod id;
mod time;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "quipu", bin_name = "qp", version, about = "Structured task substrate for agent orchestration", subcommand_required = true, arg_required_else_help = true)]
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
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("error: {e:#}");
        let code = if let Some(err) = e.downcast_ref::<db::QuipuError>() {
            match err {
                db::QuipuError::Constraint(_)  => 2,
                db::QuipuError::NotFound(_)    => 1,
                db::QuipuError::InvalidInput(_)=> 1,
            }
        } else { 1 };
        std::process::exit(code);
    }
}

fn real_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let db_path = db::resolve_path(cli.db.clone())?;
    db::warn_on_project_mismatch(&cli.db)?;
    match cli.cmd {
        Cmd::Init { prefix } => { let _ = db::open_with_prefix(&db_path, prefix.as_deref())?; println!("initialized at {}", db_path.display()); Ok(()) }
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
        #[allow(unreachable_patterns)]
        _ => { eprintln!("not implemented yet"); std::process::exit(1); }
    }
}
