//! `qp contains` — declare that one task is made up of others.
//!
#![doc = include_str!("../../docs/modules/contains.md")]

use crate::effects::{self, Effect};
use crate::outcome::Outcome;
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainsArgs {
    /// The container — the task the children are part of.
    pub parent: String,
    /// The contents. Repeatable positionally: `qp contains QP-1 QP-2 QP-3`.
    #[arg(required = true, value_name = "CHILD")]
    pub children: Vec<String>,
    /// Remove the containment edges instead of adding them.
    #[arg(long)]
    #[serde(default)]
    pub rm: bool,
    /// Required when the container is assigned/running. Must match its assignee.
    #[arg(long = "as")]
    #[serde(default)]
    pub agent: Option<String>,
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
}

impl crate::route::Served for ContainsArgs {
    const VERB: &'static str = "contains";
    type Out = ContainsOutcome;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct ContainsOutcome {
    display_id: String,
    /// Every child named on the command line, in the order given.
    children: Vec<String>,
    op: String,
    /// The children this call actually linked or released.
    changed: Vec<String>,
    /// Children this call did nothing to, because the edge was already in the
    /// requested shape or (on `--rm`) was not a containment edge at all.
    /// Reported rather than hidden so a re-run reads as "nothing to do".
    unchanged: Vec<String>,
    /// Tasks this transaction moved that the caller did not name.
    #[serde(rename = "_effects", default, skip_serializing_if = "Vec::is_empty")]
    effects: Vec<Effect>,
}
impl Outcome for ContainsOutcome {
    fn human(&self) -> String {
        // Name only what changed. Printing the full argument list next to
        // "released" claimed the release of edges that were never touched —
        // including live `blocks` edges, which `--rm` does not remove — and the
        // trailing "(… already)" read as "already released" rather than
        // "untouched". A script checking the exit code saw success either way.
        let verb = if self.op == "rm" {
            "released"
        } else {
            "contains"
        };
        let mut parts = Vec::new();
        if !self.changed.is_empty() {
            parts.push(format!(
                "{} {} {}",
                self.display_id,
                verb,
                self.changed.join(", ")
            ));
        }
        if !self.unchanged.is_empty() {
            let noun = if self.op == "rm" {
                "no containment edge"
            } else {
                "already"
            };
            parts.push(format!("{}: {}", self.unchanged.join(", "), noun));
        }
        if parts.is_empty() {
            return format!("{} unchanged", self.display_id);
        }
        parts.join("; ")
    }
}

/// Links a container to its contents, writing `dep` rows with `mode='contains'`.
///
/// # Argument direction
///
/// Storage is always depender-first: `qp contains A B` writes
/// `dep(task_id = A, depends_on_task_id = B)`, so the container depends on its
/// contents. That is what buys rollup for free — the container cannot be ready
/// while anything inside it is open, using the readiness rule that already
/// exists rather than a second one.
///
/// It also means `contains` reads *opposite* to `depends`: `qp depends X --on Y`
/// names the waiter first and `qp contains X Y...` also names the waiter first,
/// but the English runs the other way ("X contains Y" vs "X depends on Y" —
/// same direction in storage, and the parent is the waiter in both). The one
/// that genuinely inverts is `qp add --part-of`, where the *new* task is the
/// depended-on side; see `cmd::add`.
///
/// # Batching
///
/// All children land in one transaction, so a cycle introduced by the fifth
/// child rejects the whole call rather than leaving four edges applied and no
/// indication which. Ownership of the container is checked once, up front,
/// for the same reason.
pub fn exec(db_path: &std::path::Path, a: ContainsArgs) -> Result<ContainsOutcome> {
    let mut conn = db::open(db_path)?;
    let parent = id::resolve_full(&conn, &a.parent)?;
    // Resolve every child before opening the transaction: an unknown id should
    // fail as a lookup error, not as a rollback halfway through a batch.
    let children = a
        .children
        .iter()
        .map(|c| id::resolve_full(&conn, c))
        .collect::<Result<Vec<_>>>()?;

    // Every task the request itself mentions, so `_effects` can exclude them.
    let named: Vec<i64> = std::iter::once(parent.id)
        .chain(children.iter().map(|c| c.id))
        .collect();
    let via = if a.rm { "contains_rm" } else { "contains" };
    let (changed, unchanged, effects) = db::with_tx(
        &mut conn,
        |tx| -> Result<(Vec<String>, Vec<String>, Vec<Effect>)> {
            db::require_edge_owner(tx, parent.id, &parent.display_id, a.agent.as_deref())?;
            let mut changed = Vec::new();
            let mut unchanged = Vec::new();
            // Accumulated across the loop: `link_dep` sweeps after every edge,
            // so one call can move a task an earlier one did not.
            let mut effects = Vec::new();
            for child in &children {
                if a.rm {
                    let n = tx.execute(
                        "DELETE FROM dep WHERE task_id = ? AND depends_on_task_id = ? AND mode = ?",
                        rusqlite::params![parent.id, child.id, db::MODE_CONTAINS],
                    )?;
                    if n == 0 {
                        unchanged.push(child.display_id.clone());
                        continue;
                    }
                    changed.push(child.display_id.clone());
                    db::insert_event(
                        tx,
                        Some(parent.id),
                        "dep_removed",
                        a.agent.as_deref(),
                        Some(
                            &serde_json::json!({"on": child.display_id, "mode": db::MODE_CONTAINS}),
                        ),
                    )?;
                } else {
                    let (linked, moved) = db::link_dep(
                        tx,
                        parent.id,
                        &parent.display_id,
                        child.id,
                        &child.display_id,
                        db::MODE_CONTAINS,
                        a.agent.as_deref(),
                    )?;
                    effects.extend(effects::collateral(tx, &named, via, &moved)?);
                    if linked {
                        changed.push(child.display_id.clone());
                    } else {
                        unchanged.push(child.display_id.clone());
                    }
                }
            }
            // Removing the last open child can make the container ready; adding one
            // can never promote anything, and `refresh_ready` only ever promotes, so
            // running it on both paths costs a query and keeps the paths symmetric.
            let promoted = db::refresh_ready_logged(tx, a.agent.as_deref(), via)?;
            effects.extend(effects::collateral(
                tx,
                &named,
                via,
                &db::Movements::promoted(promoted),
            )?);
            Ok((changed, unchanged, effects))
        },
    )?;

    Ok(ContainsOutcome {
        display_id: parent.display_id,
        children: children.into_iter().map(|c| c.display_id).collect(),
        op: if a.rm { "rm" } else { "add" }.to_string(),
        changed,
        unchanged,
        effects,
    })
}
