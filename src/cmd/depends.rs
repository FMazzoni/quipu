//! Add or remove dependency edges between tasks.
//!
#![doc = include_str!("../../docs/modules/depends.md")]

use crate::effects::{self, Effect};
use crate::outcome::{emit, Outcome};
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

#[derive(Args, Debug)]
pub struct DependsArgs {
    /// Task that depends on another (the upstream).
    pub task: String,
    /// Task that `task` depends on (the prerequisite).
    #[arg(long = "on")]
    pub on: String,
    /// Remove the dep edge instead of adding it.
    #[arg(long)]
    pub rm: bool,
    /// Required when `task` is assigned/running. Must match the latest assignee.
    #[arg(long = "as")]
    pub agent: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct DependsOutcome {
    display_id: String,
    on_id: String,
    op: String,
    /// For `rm`: did removing this edge promote `display_id` itself to `ready`?
    /// Always `false` for `add` (adding an edge can only demote, never promote).
    ///
    /// Kept alongside `_effects` rather than folded into it: this reports on the
    /// *named* task, and `_effects` reports only on tasks the caller did not
    /// name, so neither can be derived from the other.
    promoted: bool,
    /// Tasks this transaction moved that the caller did not name.
    #[serde(rename = "_effects", default, skip_serializing_if = "Vec::is_empty")]
    effects: Vec<Effect>,
}
impl Outcome for DependsOutcome {
    fn human(&self) -> String {
        let verb = if self.op == "rm" {
            "unlinked"
        } else {
            "linked"
        };
        format!("{} {} {}", self.display_id, verb, self.on_id)
    }
}

/// Adds or removes one blocking edge.
///
/// Gated on ownership of the downstream task — see `db::require_edge_owner`.
///
/// The add path is `db::link_dep` with `MODE_BLOCKS`; `qp contains` is the same
/// call with `MODE_CONTAINS` and the arguments the other way round. The removal
/// path below is mode-blind on purpose — an edge is unlinked by its endpoints,
/// and making the caller name the mode to delete a row they can already see
/// would buy nothing.
pub fn run(db_path: &std::path::Path, a: DependsArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_resolved = id::resolve_full(&conn, &a.task)?;
    let on_resolved = id::resolve_full(&conn, &a.on)?;
    let task_id = task_resolved.id;
    let on_id = on_resolved.id;
    let (promoted, effects) = db::with_tx(&mut conn, |tx| -> Result<(bool, Vec<Effect>)> {
        db::require_edge_owner(tx, task_id, &task_resolved.display_id, a.agent.as_deref())?;

        if a.rm {
            // Read the mode before deleting so the audit event can name it.
            // `--rm` is deliberately mode-blind — an edge is identified by its
            // endpoints — but a log entry that does not say which kind of edge
            // vanished cannot be replayed against the graph.
            let removed_mode: Option<String> = tx
                .query_row(
                    "SELECT mode FROM dep WHERE task_id = ? AND depends_on_task_id = ?",
                    rusqlite::params![task_id, on_id],
                    |r| r.get(0),
                )
                .optional()?;
            let n = tx.execute(
                "DELETE FROM dep WHERE task_id = ? AND depends_on_task_id = ?",
                rusqlite::params![task_id, on_id],
            )?;
            if n == 0 {
                return Err(db::not_found(
                    format!("no dep {} → {}", a.task, a.on),
                    Some(task_resolved.display_id.clone()),
                ));
            }
            db::insert_event(
                tx,
                Some(task_id),
                "dep_removed",
                a.agent.as_deref(),
                Some(&serde_json::json!({
                    "on": on_resolved.display_id,
                    "mode": removed_mode.as_deref().unwrap_or(db::MODE_BLOCKS),
                })),
            )?;
            let promoted = db::refresh_ready_logged(tx, a.agent.as_deref(), "depends_rm")?;
            let named_returned = promoted.contains(&task_id);
            let effects = effects::collateral(
                tx,
                &[task_id, on_id],
                "depends_rm",
                &db::Movements::promoted(promoted),
            )?;
            Ok((named_returned, effects))
        } else {
            let (_, moved) = db::link_dep(
                tx,
                task_id,
                &task_resolved.display_id,
                on_id,
                &on_resolved.display_id,
                db::MODE_BLOCKS,
                a.agent.as_deref(),
            )?;
            let effects = effects::collateral(tx, &[task_id, on_id], db::MODE_BLOCKS, &moved)?;
            // Not "adding can only demote" any more. Reclassifying an edge
            // from `contains` to `blocks` takes everything under it out of the
            // frozen set, so an add can free work. `link_dep` runs both
            // reconciliation sweeps; `promoted` stays a `--rm`-only signal
            // because it reports on *this* task, which an add never promotes.
            Ok((false, effects))
        }
    })?;
    let op = if a.rm { "rm" } else { "add" };
    emit(
        a.json,
        &DependsOutcome {
            display_id: task_resolved.display_id,
            on_id: on_resolved.display_id,
            op: op.to_string(),
            promoted,
            effects,
        },
    )
}
