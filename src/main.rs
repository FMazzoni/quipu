//! `qp` — quipu CLI entry point. Parses subcommands and dispatches to `src/cmd/<name>.rs`.
//!
#![doc = include_str!("../docs/architecture.md")]
//!
#![doc = include_str!("../docs/wire-contract.md")]
//!
#![doc = include_str!("../docs/process-contract.md")]

mod client;
mod cmd;
mod db;
mod doctor;
mod effects;
mod id;
mod lease;
mod merge;
mod outcome;
mod project;
mod registry;
mod remote;
mod route;
mod s3;
mod store;
mod time;
mod wire;

use clap::{Parser, Subcommand};

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
    /// Front door to send served verbs through, instead of opening the store
    /// on this disk. `http://` is accepted for loopback literals only.
    ///
    /// The top two tiers of `resolve_server`: clap reads the flag first and
    /// `QP_SERVER` second, and the store's own recorded url is consulted only
    /// when this is `None`.
    #[arg(long, global = true, env = "QP_SERVER", value_name = "URL")]
    server: Option<String>,
    /// Project name. Repeatable, because `qp serve` opens a set of them and a
    /// client selects one from that set; a client naming more than one is a
    /// usage error rather than a silent pick.
    #[arg(
        long = "project",
        global = true,
        env = "QP_PROJECT",
        value_name = "NAME"
    )]
    project: Vec<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

/// The subcommand vocabulary. One variant per `qp` verb, each delegating to
/// `cmd::<name>::run`.
///
/// The `///` line on each variant is clap's help text, not internal
/// documentation: editing one changes what `qp --help` prints. Every variant
/// delegates its arguments to a `cmd::<name>::*Args` struct, `Init` included:
/// it runs before a store exists, but the decision it makes — start a project
/// or join one — is large enough to own a module rather than sit inline here.
#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Initialize a store in the current directory
    Init(cmd::init::InitArgs),
    /// Add a new task
    Add(cmd::add::AddArgs),
    /// Assign a ready task to an agent (orchestrator only)
    Assign(cmd::assign::AssignArgs),
    /// Claim an assigned task as the running agent
    Claim(cmd::claim::ClaimArgs),
    /// Atomically take the next ready task (select + assign in one statement)
    Take(cmd::take::TakeArgs),
    /// Complete a running task (records decisions/artifacts)
    Complete(cmd::complete::CompleteArgs),
    /// Mark an assigned/running task blocked with a reason (→ pending)
    Block(cmd::block::BlockArgs),
    /// Cancel a task (any non-terminal state)
    Cancel(cmd::cancel::CancelArgs),
    /// Agent self-release of a claim (assigned/running → pending)
    Abandon(cmd::abandon::AbandonArgs),
    /// Orchestrator force-release of a claim (assigned/running → pending)
    Reclaim(cmd::reclaim::ReclaimArgs),
    /// Log a free-form event against a task
    Log(cmd::log::LogArgs),
    /// Manage tags on a task
    Tag(cmd::tag::TagArgs),
    /// Manage relations between tasks
    Relation(cmd::relation::RelationArgs),
    /// Attach this store to a claim authority / ledger, or detach to local-only
    Remote(cmd::remote::RemoteArgs),
    /// Name this store and manage the machine-local name → store registry
    Project(cmd::project::ProjectArgs),
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
    /// Declare that a task is made up of others (containment edges)
    Contains(cmd::contains::ContainsArgs),
    /// Mutate task fields (title, tier, description)
    Edit(cmd::edit::EditArgs),
    /// Emit a structured JSON snapshot of the store
    Report(cmd::report::ReportArgs),
    /// Serialise the event log to append-only JSONL batches on disk
    Export(cmd::export::ExportArgs),
    /// Restore or merge a store from exported JSONL batches
    Import(cmd::import::ImportArgs),
    /// Exchange the ledger with the configured remote (explicit; never automatic)
    Sync(cmd::sync::SyncArgs),
    /// Show a single-ticket detail view (human or JSON)
    Show(cmd::show::ShowArgs),
    /// Read every project on the shared ledger (read-only; writes nothing local)
    Ledger(cmd::ledger::LedgerArgs),
    /// Front the resolved stores with an HTTP listener, in the foreground
    Serve(cmd::serve::ServeArgs),
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
        Cmd::Init(a) => a.json,
        Cmd::Add(a) => a.json,
        Cmd::Assign(a) => a.json,
        Cmd::Claim(a) => a.json,
        Cmd::Take(a) => a.json,
        Cmd::Complete(a) => a.json,
        Cmd::Block(a) => a.json,
        Cmd::Cancel(a) => a.json,
        Cmd::Abandon(a) => a.json,
        Cmd::Reclaim(a) => a.json,
        Cmd::Log(a) => a.json,
        Cmd::Tag(a) => a.json,
        Cmd::Depends(a) => a.json,
        Cmd::Contains(a) => a.json,
        Cmd::Edit(a) => a.json,
        Cmd::Show(a) => a.json,
        Cmd::Report(a) => a.json,
        Cmd::Export(a) => a.json,
        Cmd::Import(a) => a.json,
        Cmd::Sync(a) => a.json,
        Cmd::Ledger(a) => a.json,
        Cmd::Relation(a) => a.json(),
        Cmd::Remote(a) => a.json(),
        Cmd::Project(a) => a.json(),
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
        // `serve` writes JSON Lines to stderr for its whole life and has no
        // success payload to print, so a `--json` flag would name nothing.
        Cmd::Wait(_) | Cmd::InstallSkills(_) | Cmd::Serve(_) => false,
    }
}

/// Whether this subcommand needs a store to already exist.
///
/// `init` is the only command allowed to create one. `install-skills` writes
/// files on disk and never opens a store at all, so requiring one would break
/// the bootstrap step that runs *before* a project exists.
///
/// `serve` is the fourth: it resolves a *set* of stores, and the missing-store
/// answer it owes an operator names which project failed and why. The check
/// still happens — `db::open` runs per resolved store before the listener binds
/// — it just happens where the name is known.
///
/// The registry readers are the third case and the least obvious: `qp project
/// list`, `lookup` and `unregister` answer from the registry file alone and are
/// the commands you reach for *because* you do not know which store you are
/// near — gating them on being attached to one would make the tool that
/// diagnoses a lost project unusable from anywhere a project is lost. Only
/// `register` reads the store, because only it needs the name and uuid to file.
fn needs_store(cmd: &Cmd) -> bool {
    !matches!(
        cmd,
        Cmd::Init(_)
            | Cmd::InstallSkills(_)
            | Cmd::Serve(_)
            | Cmd::Project(cmd::project::ProjectArgs {
                op: cmd::project::ProjectOp::List { .. }
                    | cmd::project::ProjectOp::Lookup { .. }
                    | cmd::project::ProjectOp::Unregister { .. },
            })
    )
}

/// Restores the default disposition for `SIGPIPE`.
///
/// That is what makes `qp` behave like a normal Unix filter when a reader
/// closes early.
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

/// Parses, dispatches, and renders the result.
///
/// Whatever error comes back becomes the process exit code and the
/// `{"error": ...}` envelope.
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

/// Says that `--server` was set and this verb still read the local store.
///
/// Follows `warn_on_project_mismatch`'s shape, including the format switch:
/// under `--json` stderr is JSON Lines, so a prose line here would make the
/// error envelope on the next line unparseable.
fn warn_unserved(json: bool) {
    if json {
        eprintln!(
            "{}",
            serde_json::json!({"warning": {
                "kind": "verb_not_served",
                "message": "--server is set but this verb is not served yet; it ran against the local store",
            }})
        );
    } else {
        eprintln!(
            "warning: --server is set but this verb is not served yet; it ran \
             against the local store"
        );
    }
}

/// Refuses a poll that would have answered from the wrong store. Exit 1.
///
/// **The bug this closes, stated as the orchestrator sees it.** `wait` and
/// `watch` have no route, so with a front door resolved they polled the store
/// on *this* disk — where a cohort the operator never filed is trivially empty.
/// `qp wait --tag wave:1 --empty` returned 0 in seven milliseconds against a
/// front door holding six open tickets, and an orchestrator branching on that
/// exit code proceeds to the next phase with every one of them still open.
/// Stdout is empty in both the true and the false case, so the only signal was
/// prose on a stderr that orchestration scripts routinely discard.
///
/// Three things about the shape, each of which was a choice:
///
/// - **It refuses rather than warns.** `warn_unserved` is right for `export`
///   and `sync`: those name a path on the caller's disk, so running locally is
///   what the operator meant and the warning says which store answered. A
///   barrier has no such reading — its whole output is an exit code, and an
///   exit code cannot carry a caveat.
/// - **It keys on the *resolved* front door, not on the explicit flag.** The
///   recorded-url tier is exactly the case that produced no warning at all:
///   `resolve_server` did not consult it for an unserved verb, so a configured
///   store answered locally in complete silence. `is_client_poll` puts these
///   two verbs on the served side of that gate, which is what makes a recorded
///   url reach this refusal.
/// - **It runs before the attach check**, so a storeless client and a client
///   that holds a store get the same answer. The betrayed case is the second
///   one — a storeless client already got `no_store` — and two different exit
///   codes for one misconfiguration is a distinction nothing downstream wants.
fn refuse_client_poll(cmd: &Cmd, base: &str) -> anyhow::Error {
    let (verb, wrong) = match cmd {
        Cmd::Wait(_) => (
            "wait",
            "the cohort you named is trivially empty there, so the barrier \
             releases in milliseconds with every ticket still open",
        ),
        _ => (
            "watch",
            "its event log is a different log, so the tail is indistinguishable \
             from a real one and is not the front door's",
        ),
    };
    db::invalid_input(format!(
        "`qp {verb}` is not served, and a front door is configured at `{base}`. \
         Answering would mean polling the store on this disk instead, and {wrong}. \
         Point at the store you mean: unset `--server`/`QP_SERVER`, or run \
         `qp remote clear --front-door` if this store records one."
    ))
}

/// The front door this invocation talks to, in four tiers.
///
/// ```text
/// --server <url>            explicit, this invocation
/// QP_SERVER=<url>           explicit, this environment
/// meta['remote.server']     recorded in THIS store
/// (none)                    open the store on this disk, as always
/// ```
///
/// **Why this order, and not any other.** The two explicit tiers come first
/// because they are the escape hatch: an operator diagnosing a store must be
/// able to override what that store says about itself, and a recorded setting
/// that outranked a typed flag would leave no way to reach a second front door
/// from a configured machine. Clap already implements the first over the second
/// — the flag beats its own `env` — so nothing here re-implements it, and the
/// two are one `Option` by the time this function sees them. The recorded url
/// comes third rather than not at all because a setting nothing reads on the
/// path that matters is decorative: a machine looked configured to
/// `qp remote doctor` while every command quietly answered from the local
/// store, which is the silent-wrong-store failure the attach check exists to
/// prevent, arriving from the opposite direction. Local last, unchanged, is
/// what every existing store does.
///
/// **An explicit `--server` that disagrees with the recorded url wins in
/// silence, and that is a decision rather than an omission.** The precedent for
/// the other choice is [`crate::db::warn_on_project_mismatch`], and reading it
/// is what settles this: that warning fires only when the path came from
/// `--db`/`QP_DB`, only on a genuine `project_uuid` difference, and only
/// between two paths it had *already* resolved. Here the second value is not
/// already in hand — computing it means opening the local store, which is
/// exactly what an explicit `--server` exists to avoid, and the dominant
/// deployment for one is a machine with no local store at all. So the warning
/// could only fire on the machines that least need it and would be silent on
/// the ones that do. What that precedent guards against is a store the operator
/// did not name being read silently; here the operator named the front door on
/// the command line and the answer comes from it.
///
/// **Tier 3 cannot configure the machine the front door exists for, and this
/// slice did not change that.** The recorded url is a row in `meta`, so reading
/// it needs a store on this disk. A second machine — one that has no store,
/// which is the whole reason to front one over HTTP — has no `meta` to read, so
/// tier 3 can never fire there and its channel is still `QP_SERVER` in the
/// environment, plus `--project` when the front door holds more than one store.
/// What tier 3 buys is the machine that *does* hold the store: a coordinator
/// that wants its own commands to go through the front door its agents use, so
/// that one board is one board. That is worth having and it is not the same
/// thing, and the difference must not be read off a green `qp remote doctor`.
///
/// The storeless machine has an obvious home for a url and it is deliberately
/// not used here: `registry.rs` is a real per-machine file with an `Entry` per
/// project, and its own rules say a purely additive field needs no version
/// bump. Putting the url there would make a storeless machine configurable by
/// name, and it is a fourth tier with its own precedence question (a registry
/// entry and a store that disagree), its own `qp project register` surface, and
/// its own doctor sequence. It is a slice, not a line, and it is the one to cut
/// next.
///
/// **The recorded url is read only for a verb the front door actually
/// answers.** Two things follow, both load-bearing. An unserved verb — `wait`,
/// `watch`, `export`, and every op of `remote` and `project` — never pays the
/// read and never changes behaviour, which is what keeps `qp remote doctor`,
/// `qp remote show` and `qp remote clear --front-door` working as the escape
/// hatches from a front door that is down: they are structurally incapable of
/// going over the wire. And `warn_unserved` below still keys on the *explicit*
/// flag, so it cannot start firing on every local command in a configured
/// store.
fn resolve_server(
    explicit: Option<String>,
    cmd: &Cmd,
    db_path: &std::path::Path,
) -> anyhow::Result<Option<String>> {
    if explicit.is_some() {
        return Ok(explicit);
    }
    if !route::is_served(cmd) && !route::is_client_poll(cmd) {
        return Ok(None);
    }
    remote::recorded_server(db_path)
}

/// Dispatch, split out from `main` so every arm can use `?`.
///
/// Errors are returned rather than printed: `main` is the single place that
/// maps a `QuipuError` to an exit code and an output format, so no command
/// module needs to know either. Path resolution, the attach check and the
/// project-mismatch warning happen once here, ahead of the match, rather than
/// in each arm.
///
/// Resolving a store path and creating one are separate acts. `resolve_path`
/// answers "where would the store be" and cannot fail; `require_attached` then
/// refuses every command but `init` when nothing is there. Before that split a
/// misconfigured machine got a silent private store and exit 0 — `qp list`
/// printed `[]`, `qp add` filed into a project nobody meant to make, and only
/// `qp show` complained, as a not-found that read like a bad ticket id.
fn real_main(cli: Cli, json: bool) -> anyhow::Result<()> {
    let db_path = db::resolve_path(cli.db.clone())?;
    let server = resolve_server(cli.server.clone(), &cli.cmd, &db_path)?;
    // A client pointed at a front door has no store on this disk and must not
    // be refused for lacking one. `route::is_served` is generated from the same
    // list the dispatch arms below use, so the two cannot drift apart.
    let remote = server.is_some() && (route::is_served(&cli.cmd) || route::is_client_poll(&cli.cmd));
    // Before the attach check, so the refusal is the same answer whether or not
    // this machine happens to hold a store. See `refuse_client_poll`.
    if let Some(base) = server.as_deref() {
        if route::is_client_poll(&cli.cmd) {
            return Err(refuse_client_poll(&cli.cmd, base));
        }
    }
    if needs_store(&cli.cmd) && !remote {
        db::require_attached(&db_path, cli.db.is_some())?;
    }
    // `json` is threaded in so the warning matches the stream's format: under
    // --json stderr is JSON Lines, so a prose warning here would make the
    // error envelope on the following line unparseable. See QP-120.
    if !remote {
        db::warn_on_project_mismatch(&cli.db, json)?;
    }
    // A verb the front door does not answer yet still runs, against the store on
    // this disk — which is a *different store* from the one the operator just
    // named. Silently reading the wrong file is the exact failure the attach
    // check exists to prevent, so it is said out loud. The line disappears verb
    // by verb as the fan-out slices land.
    //
    // `cli.server`, deliberately, and NOT the resolved `server`. The two agree
    // for an unserved verb today — `resolve_server` never consults the recorded
    // url for one — so this reads as a distinction without a difference, and it
    // is the difference that matters: a store with a front door recorded would
    // otherwise print "this ran against the local store" on every `qp wait`,
    // `watch`, `export`, `sync`, `ledger` and `import`, forever, on the one
    // machine that holds the file. A warning that fires on every command in a
    // correct configuration is one people learn to filter out, and then it is
    // not there on the day it is true. The condition to keep is: the operator
    // named a front door for THIS invocation and it was not used.
    if cli.server.is_some() && !remote && needs_store(&cli.cmd) {
        warn_unserved(json);
    }
    if cli.project.len() > 1 && !matches!(cli.cmd, Cmd::Serve(_)) {
        return Err(db::invalid_input(
            "--project may be given more than once only for `qp serve`",
        ));
    }
    let ctx = route::Ctx {
        db_path: db_path.clone(),
        server,
        project: cli.project.first().cloned(),
    };
    match cli.cmd {
        Cmd::Init(a) => cmd::init::run(&db_path, a),
        Cmd::Add(a) => route::dispatch::<cmd::add::AddArgs>(&ctx, a),
        Cmd::Assign(a) => route::dispatch::<cmd::assign::AssignArgs>(&ctx, a),
        Cmd::Claim(a) => route::dispatch::<cmd::claim::ClaimArgs>(&ctx, a),
        Cmd::Take(a) => route::dispatch::<cmd::take::TakeArgs>(&ctx, a),
        Cmd::Complete(a) => route::dispatch::<cmd::complete::CompleteArgs>(&ctx, a),
        Cmd::Block(a) => route::dispatch::<cmd::block::BlockArgs>(&ctx, a),
        Cmd::Cancel(a) => route::dispatch::<cmd::cancel::CancelArgs>(&ctx, a),
        Cmd::Abandon(a) => route::dispatch::<cmd::abandon::AbandonArgs>(&ctx, a),
        Cmd::Reclaim(a) => route::dispatch::<cmd::reclaim::ReclaimArgs>(&ctx, a),
        Cmd::Log(a) => route::dispatch::<cmd::log::LogArgs>(&ctx, a),
        Cmd::Tag(a) => route::dispatch::<cmd::tag::TagArgs>(&ctx, a),
        Cmd::Relation(a) => route::dispatch::<cmd::relation::RelationArgs>(&ctx, a),
        Cmd::Remote(a) => cmd::remote::run(&db_path, a),
        Cmd::Project(a) => cmd::project::run(&db_path, a),
        Cmd::Timeline(a) => route::dispatch::<cmd::timeline::TimelineArgs>(&ctx, a),
        Cmd::Decisions(a) => route::dispatch::<cmd::decisions::DecisionsArgs>(&ctx, a),
        Cmd::Tree(a) => route::dispatch::<cmd::tree::TreeArgs>(&ctx, a),
        Cmd::Status(a) => route::dispatch::<cmd::status::StatusArgs>(&ctx, a),
        Cmd::List(a) => route::dispatch::<cmd::list::ListArgs>(&ctx, a),
        Cmd::Wave(a) => route::dispatch::<cmd::wave::WaveArgs>(&ctx, a),
        Cmd::Wait(a) => cmd::wait::run(&db_path, a),
        Cmd::Watch(a) => cmd::watch::run(&db_path, a),
        Cmd::InstallSkills(a) => cmd::install_skills::run(a),
        Cmd::Depends(a) => route::dispatch::<cmd::depends::DependsArgs>(&ctx, a),
        Cmd::Contains(a) => route::dispatch::<cmd::contains::ContainsArgs>(&ctx, a),
        Cmd::Edit(a) => route::dispatch::<cmd::edit::EditArgs>(&ctx, a),
        // Not `dispatch`: `--output` names a file on the caller's disk, so the
        // write stays client-side however the payload was obtained.
        Cmd::Report(a) => cmd::report::run(&ctx, a),
        Cmd::Export(a) => cmd::export::run(&db_path, a),
        Cmd::Import(a) => cmd::import::run(&db_path, a),
        Cmd::Sync(a) => cmd::sync::run(&db_path, a),
        Cmd::Show(a) => route::dispatch::<cmd::show::ShowArgs>(&ctx, a),
        Cmd::Ledger(a) => cmd::ledger::run(&db_path, a),
        Cmd::Serve(a) => cmd::serve::run(&db_path, a, &cli.project),
        #[allow(unreachable_patterns)]
        _ => {
            eprintln!("not implemented yet");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// clap's own assertions over the whole command tree, run as a test.
    ///
    /// The class this catches at its cause: a `global = true` flag whose long
    /// name a subcommand also declares. Clap refuses that with a
    /// `debug_assert`, so a *release* build does not error — it silently
    /// mis-resolves, and the first read of the field panics with a downcast
    /// mismatch nowhere near the flag. That has already happened here, which is
    /// why `qp remote set` spells its url `--front-door` and not `--server`.
    /// `debug_assert()` runs those same assertions on demand, in a debug test
    /// binary, over every subcommand at once.
    #[test]
    fn the_command_tree_has_no_internal_contradiction() {
        Cli::command().debug_assert();
    }
}
