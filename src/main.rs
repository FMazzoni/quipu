//! `qp` — quipu CLI entry point. Parses subcommands and dispatches to `src/cmd/<name>.rs`.
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

/// Top-level parse target: the global `--db` override plus the chosen subcommand.
///
/// `--db` is the only `global = true` flag, so it is accepted before or after
/// the subcommand and falls back to `QP_DB`. `--json` is deliberately *not*
/// global — see `wants_json` for why it is redeclared per command instead.
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

/// The subcommand vocabulary. One variant per `qp` verb, each delegating to
/// `cmd::<name>::run`.
///
/// The `///` line on each variant is clap's help text, not internal
/// documentation: editing one changes what `qp --help` prints. `Init` is the
/// only variant that carries its arguments inline rather than in a
/// `cmd::<name>::*Args` struct, because it is the only command that runs
/// before a store exists and so has no `cmd` module to own them.
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

/// Success output for `qp init`, the one command whose `Outcome` lives here
/// rather than in a `cmd` module.
///
/// `prefix` is read back out of the store after `db::init` rather than echoing
/// `--prefix`, because re-initializing an existing store keeps the original
/// prefix and ignores the flag — echoing the argument would report a rename
/// that did not happen.
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
///
/// **Invariant: if a variant's args carry a `json` field, this must return it.**
/// Collapsing a variant into a `_ => false` arm is silent and survives review —
/// the command still emits JSON on success, so only the failure path diverges,
/// and a `--json` consumer that hits an error gets prose it cannot parse.
/// That was QP-158: seven read-only commands (`list`, `tree`, `timeline`,
/// `wave`, `status`, `decisions`, `watch`) sat in one such arm and broke the
/// "stderr is JSON Lines" contract exactly when a consumer most needed it.
/// The match is deliberately exhaustive with no wildcard so a new command
/// cannot inherit the bug by default — adding a variant fails to compile until
/// its `--json` story is decided.
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
        Cmd::Tree(a) => a.json,
        Cmd::Timeline(a) => a.json,
        Cmd::Wave(a) => a.json,
        Cmd::Status(a) => a.json,
        Cmd::List(a) => a.json,
        Cmd::Decisions(a) => a.json,
        Cmd::Watch(a) => a.json,
        // The only two variants that are genuinely `false`: neither declares a
        // `--json` flag, so there is no flag to read. Giving `wait` one would be
        // a contract change, not a bug fix — its signal is the exit code
        // (4 = empty cohort), not its output.
        Cmd::Wait(_) | Cmd::InstallSkills(_) => false,
    }
}

/// Restores the default disposition for `SIGPIPE`, making `qp` behave like a
/// normal Unix filter when a reader closes early.
///
/// The Rust runtime sets `SIGPIPE` to `SIG_IGN` before `main`, so a write to a
/// closed pipe returns `EPIPE` and the `println!` family *panics* — `qp show X
/// | head -1` exited 101, a code outside the documented contract (QP-139). It
/// looked intermittent because it is a race between our flushes and the
/// reader's close, not a buffer-size threshold: the same command with byte-
/// identical output panicked 15 times in 20. Commands that write once (`status`)
/// never tripped it, which is why it hid for so long — that, and `$?` after a
/// pipeline reporting the *reader's* status, so casual checks showed 0.
///
/// Restoring `SIG_DFL` is one call that covers every present and future output
/// path. Handling `EPIPE` at each call site was rejected: it is the same fix
/// spread across every `cmd` module, and the ticket's own framing ("easy to miss
/// a path") is the failure mode. The consequence is that `qp` is now *killed by
/// signal 13* rather than exiting — wait-status semantics, not an exit code, but
/// shells surface it as 141, so the exit-code tables must say so.
///
/// Must run before any output. Safety: `signal` with `SIG_DFL` is
/// async-signal-safe and this is single-threaded startup, before any writes.
#[cfg(unix)]
fn restore_sigpipe_default() {
    // SAFETY: setting a signal disposition to SIG_DFL is always well-defined,
    // and no other thread exists yet to observe the change.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_sigpipe_default() {}

/// Renders a clap parse failure through the same contract as every other error.
///
/// Clap exits the process itself on a parse failure, before `real_main` runs, so
/// a typo bypassed the error envelope entirely: `qp wait --timeout 1 --json`
/// printed bare prose to stderr and exited 2 — the code documented as "conflict,
/// retry may succeed". A skill retrying on 2 looped forever on a typo, and under
/// `--json` it was parsing stderr that was not JSON Lines (QP-150).
///
/// The fix is not a new exit code. A parse failure *is* bad CLI input, and
/// `QuipuError::InvalidInput` already maps to exit 1 in the published table, so
/// routing usage errors there makes 1 and 2 mean what the docs always claimed
/// rather than redefining either. Giving usage errors a code of their own (64 /
/// `EX_USAGE`) was rejected for that reason: it would add a row to a contract
/// that agents branch on, to fix a problem the existing rows already describe.
///
/// `--json` is sniffed from the raw argv because parsing is precisely what
/// failed — `wants_json` needs a parsed `Cmd` that does not exist here.
fn handle_parse_error(err: clap::Error) -> ! {
    use clap::error::ErrorKind;
    // `--help`/`--version` are successful requests for output, not failures:
    // clap prints them to stdout and the process exits 0.
    if matches!(
        err.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        err.exit();
    }
    let json = std::env::args().any(|a| a == "--json");
    if json {
        // Clap's rendering is a multi-line block with a usage hint; the envelope
        // carries the first line, which is the diagnosis. The rest is help text
        // that belongs on a terminal, not in a machine-read `message` field.
        let rendered = err.render().to_string();
        let message = rendered
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("invalid arguments")
            .trim_start_matches("error: ")
            .to_string();
        let body = serde_json::json!({"kind": "invalid_input", "message": message});
        eprintln!("{}", serde_json::json!({"error": body}));
    } else {
        let _ = err.print();
    }
    std::process::exit(1);
}

/// Parses, dispatches, and renders whatever error comes back as the process
/// exit code and the `{"error": ...}` envelope.
fn main() {
    restore_sigpipe_default();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => handle_parse_error(e),
    };
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

/// Dispatch, split out from `main` so every arm can use `?`.
///
/// Errors are returned rather than printed: `main` is the single place that
/// maps a `QuipuError` to an exit code and an output format, so no command
/// module needs to know either. Path resolution and the project-mismatch
/// warning happen once here, ahead of the match, rather than in each arm.
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
