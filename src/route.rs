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
    let out: S::Out = match &ctx.server {
        Some(base) => {
            let body = serde_json::to_value(&a)?;
            let v = crate::client::call(base, S::VERB, ctx.project.as_deref(), body)?;
            serde_json::from_value(v).map_err(|e| {
                anyhow::anyhow!(
                    "`{}` response is not a payload this client understands: {e}",
                    S::VERB
                )
            })?
        }
        None => S::exec(&ctx.db_path, a)?,
    };
    emit(json, &out)
}

/// Declares the served verbs once, for the server table and the client gate.
///
/// A fan-out slice adds a verb by adding one line here and flipping that
/// verb's arm in `real_main` from `cmd::<v>::run` to `route::dispatch::<_>`.
/// Nothing anywhere gains a new match arm, which is the point: two branches
/// inserting adjacent single lines into one 30-arm match is the worst merge
/// shape there is, and it has already happened in this repository.
macro_rules! served {
    ($($variant:ident => $args:path),+ $(,)?) => {
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
        pub(crate) fn is_served(cmd: &crate::Cmd) -> bool {
            matches!(cmd, $(crate::Cmd::$variant(_))|+)
        }
    };
}

served! {
    Abandon => crate::cmd::abandon::AbandonArgs,
    Add => crate::cmd::add::AddArgs,
    Assign => crate::cmd::assign::AssignArgs,
    Block => crate::cmd::block::BlockArgs,
    Cancel => crate::cmd::cancel::CancelArgs,
    Claim => crate::cmd::claim::ClaimArgs,
    Complete => crate::cmd::complete::CompleteArgs,
    Contains => crate::cmd::contains::ContainsArgs,
    Depends => crate::cmd::depends::DependsArgs,
    Edit => crate::cmd::edit::EditArgs,
    Log => crate::cmd::log::LogArgs,
    Reclaim => crate::cmd::reclaim::ReclaimArgs,
    Relation => crate::cmd::relation::RelationArgs,
    Tag => crate::cmd::tag::TagArgs,
    Take => crate::cmd::take::TakeArgs,
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
                "abandon", "add", "assign", "block", "cancel", "claim", "complete", "contains",
                "depends", "edit", "log", "reclaim", "relation", "tag", "take",
            ],
            "every write verb the wire contract's triage lists as served, and \
             nothing else"
        );
        for v in &verbs {
            assert!(lookup(v).is_some(), "{v} is in ROUTES but not resolvable");
        }
        assert!(lookup("init").is_none(), "`init` must never be servable");
    }
}
