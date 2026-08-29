//! The served-verb table, as data rather than as hand-written match arms.
//!
#![doc = include_str!("../docs/modules/route.md")]

use crate::outcome::{emit, Outcome};
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// A command that can run behind the front door as well as in this process.
///
/// Implementing it is the whole of what makes a verb servable: the args become
/// the request body, the payload becomes the response body, and both halves of
/// the dispatch below are then generic over it.
pub(crate) trait Served: clap::Args + Serialize + DeserializeOwned + Sized {
    /// The path segment under [`crate::wire::API_PREFIX`]. Matches the CLI verb.
    const VERB: &'static str;
    /// The success payload.
    ///
    /// `DeserializeOwned` is what lets the *client* render `human()` from
    /// bytes the server never rendered.
    type Out: Outcome + DeserializeOwned;
    /// The `--json` flag, read before the args are consumed.
    ///
    /// It is a property of the invoking terminal and is `#[serde(skip)]` on
    /// every args struct, so it never crosses the socket.
    fn wants_json(&self) -> bool;
    /// The store-side half: open, one `with_tx`, return the payload.
    ///
    /// This is the old `run` minus the `emit`, and nothing in it knows about
    /// HTTP.
    fn exec(db_path: &Path, a: Self) -> Result<Self::Out>;
    /// Puts the caller's rendering options back onto a payload from the wire.
    ///
    /// Only `#[serde(skip)]` fields may be touched: the server never sees them,
    /// so a rendering flag never enters the protocol. `--with-description` and
    /// `--show-tags` change how many lines `human()` prints and nothing about
    /// the data, and a response that differed by them would make the body
    /// depend on how the caller's tty was going to render it — the same
    /// argument that keeps `--json` off the wire. Empty for every verb whose
    /// human view has no options; the local path never needs it, because
    /// `exec` still has the args in hand.
    fn restore_view(&self, _out: &mut Self::Out) {}
}

/// What the server does with one request body, with the verb's types erased.
pub(crate) type Handler = fn(&Path, Value) -> Result<Value>;

/// One row of the table the server routes on.
pub(crate) struct Route {
    pub verb: &'static str,
    pub handler: Handler,
}

/// The erased handler for one verb. One monomorphisation per row, no arms.
fn handle<S: Served>(db_path: &Path, body: Value) -> Result<Value> {
    let a: S = serde_json::from_value(body).map_err(|e| {
        crate::db::invalid_input(format!("`{}` request body is not valid: {e}", S::VERB))
    })?;
    Ok(serde_json::to_value(S::exec(db_path, a)?)?)
}

/// Everything a dispatched verb needs that is not its own arguments.
///
/// `server` being `Some` is the only thing that decides local versus remote,
/// which is why no command module has to know either exists.
pub(crate) struct Ctx {
    pub db_path: PathBuf,
    pub server: Option<String>,
    pub project: Option<String>,
}

/// Runs one served verb, locally or through the front door, and prints it.
///
/// The two branches converge on the same `S::Out`, so `human()` is rendered
/// here in the client either way — the server sends the structured payload and
/// never the prose. See the wire contract's "Human output ownership".
pub(crate) fn dispatch<S: Served>(ctx: &Ctx, a: S) -> Result<()> {
    let json = a.wants_json();
    let out = fetch(ctx, a)?;
    emit(json, &out)
}

/// Produces one payload, locally or through the front door, and prints nothing.
///
/// Split out of `dispatch` for the one verb whose result does not go to stdout:
/// `report --output <PATH>` names a file on the *caller's* disk, so the write
/// has to happen client-side however the payload was obtained. Exactly one
/// round trip either way — the whole read side rests on that, and the shape of
/// this function is why it is structural rather than a rule to remember.
pub(crate) fn fetch<S: Served>(ctx: &Ctx, a: S) -> Result<S::Out> {
    match &ctx.server {
        Some(base) => {
            let body = serde_json::to_value(&a)?;
            let v = crate::client::call(base, S::VERB, ctx.project.as_deref(), body)?;
            let mut out: S::Out = serde_json::from_value(v).map_err(|e| {
                anyhow::anyhow!(
                    "`{}` response is not a payload this client understands: {e}",
                    S::VERB
                )
            })?;
            a.restore_view(&mut out);
            Ok(out)
        }
        None => S::exec(&ctx.db_path, a),
    }
}

/// Declares the served verbs once, for the server table and the client gate.
///
/// A fan-out slice adds a verb by adding one line here and flipping that
/// verb's arm in `real_main` from `cmd::<v>::run` to `route::dispatch::<_>`.
/// Nothing anywhere gains a new match arm, which is the point: two branches
/// inserting adjacent single lines into one 30-arm match is the worst merge
/// shape there is, and it has already happened in this repository.
macro_rules! served {
    ($($variant:ident $(if $guard:path)? => $args:path),+ $(,)?) => {
        /// Every verb the front door answers, in one table.
        pub(crate) const ROUTES: &[Route] = &[$(
            Route { verb: <$args as Served>::VERB, handler: handle::<$args> },
        )+];

        /// Whether `--server` sends this command over the wire.
        ///
        /// Gates the attach check in `real_main`: a client pointed at a server
        /// has no local store and must not be refused for lacking one. It
        /// therefore has to name exactly the set `real_main` actually
        /// dispatches remotely, which is why it is generated from this list
        /// rather than written out beside it.
        ///
        /// The optional `if <path>` guard is for a subcommand group the front
        /// door answers only part of: `qp relation list` is served while
        /// `relation add`/`rm` are not, and one `Cmd` variant carries all
        /// three. Without the guard this would claim `--server qp relation add`
        /// goes over the wire, and `real_main` would then run it against the
        /// local store with the attach check already skipped — reading and
        /// writing the wrong file, silently, which is the exact failure the
        /// check exists to prevent.
        pub(crate) fn is_served(cmd: &crate::Cmd) -> bool {
            #[allow(unused_variables)]
            match cmd {
                $( crate::Cmd::$variant(a) => true $( && $guard(a) )?, )+
                _ => false,
            }
        }
    };
}

served! {
    Add => crate::cmd::add::AddArgs,
    Assign => crate::cmd::assign::AssignArgs,
    Claim => crate::cmd::claim::ClaimArgs,
    Complete => crate::cmd::complete::CompleteArgs,
    List => crate::cmd::list::ListArgs,
    Show => crate::cmd::show::ShowArgs,
    Tree => crate::cmd::tree::TreeArgs,
    Timeline => crate::cmd::timeline::TimelineArgs,
    Wave => crate::cmd::wave::WaveArgs,
    Status => crate::cmd::status::StatusArgs,
    Report => crate::cmd::report::ReportArgs,
    Decisions => crate::cmd::decisions::DecisionsArgs,
    Relation if crate::cmd::relation::is_list => crate::cmd::relation::RelationListArgs,
}

/// The handler for a verb, or `None` if the front door does not answer it.
pub(crate) fn lookup(verb: &str) -> Option<Handler> {
    ROUTES.iter().find(|r| r.verb == verb).map(|r| r.handler)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is keyed by the verb the CLI spells, with no duplicates.
    #[test]
    fn routes_are_unique_and_named_after_their_verbs() {
        let mut verbs: Vec<&str> = ROUTES.iter().map(|r| r.verb).collect();
        let n = verbs.len();
        verbs.sort_unstable();
        verbs.dedup();
        assert_eq!(verbs.len(), n, "two routes share a verb");
        assert_eq!(
            verbs,
            [
                "add",
                "assign",
                "claim",
                "complete",
                "decisions",
                "list",
                "relation/list",
                "report",
                "show",
                "status",
                "timeline",
                "tree",
                "wave",
            ]
        );
        for v in &verbs {
            assert!(lookup(v).is_some(), "{v} is in ROUTES but not resolvable");
        }
        assert!(lookup("init").is_none(), "`init` must never be servable");
    }
}
