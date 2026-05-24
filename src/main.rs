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
    Init,
    /// Add a new task
    Add(cmd::add::AddArgs),
    /// Assign a ready task to an agent (orchestrator only)
    Assign(cmd::assign::AssignArgs),
    /// Claim an assigned task as the running agent
    Claim(cmd::claim::ClaimArgs),
    /// Complete a running task (records decisions/artifacts)
    Complete,
    /// Mark a running task blocked with a reason
    Block,
    /// Cancel a task (any non-terminal state)
    Cancel,
    /// Agent self-release of a claim (running → ready)
    Abandon,
    /// Orchestrator force-release of a claim
    Reclaim,
    /// Log a free-form event against a task
    Log,
    /// Manage tags on a task
    Tag,
    /// Manage relations between tasks
    Relation,
    /// Render the task DAG
    Tree,
    /// Show the event timeline (per-task or global)
    Timeline,
    /// Show the current wave: ready / running / blocked groups
    Wave,
    /// Snapshot of state counts
    Status,
    /// List tasks with filters
    List,
    /// Decision events (filter alias over timeline)
    Decisions,
    /// Block until a tag/state cohort drains
    Wait,
    /// Tail new events as they are recorded
    Watch,
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
        Cmd::Init => { let _ = db::open(&db_path)?; println!("initialized at {}", db_path.display()); Ok(()) }
        Cmd::Add(a) => cmd::add::run(&db_path, a),
        Cmd::Assign(a) => cmd::assign::run(&db_path, a),
        Cmd::Claim(a) => cmd::claim::run(&db_path, a),
        _ => { eprintln!("not implemented yet"); std::process::exit(1); }
    }
}
