//! The terminal edge: any non-terminal state → `cancelled`.
//!
#![doc = include_str!("../../docs/modules/cancel.md")]

use crate::outcome::{emit, Outcome};
use crate::{claimref, db, id};
use anyhow::Result;
use clap::Args;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct CancelArgs {
    pub task: String,
    #[arg(long)]
    pub reason: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct Cancelled {
    display_id: String,
    state: String,
    reason: Option<String>,
}
impl Outcome for Cancelled {
    fn human(&self) -> String {
        format!("{} cancelled", self.display_id)
    }
}

pub fn run(db_path: &std::path::Path, a: CancelArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    // Resolved before the transaction: the ref is deleted only once the
    // local release has committed. `None` on a local-only store, which is
    // what keeps this edge offline and unchanged.
    let release = claimref::release_for(db_path, &conn, task_id, &resolved.display_id)?;
    db::with_tx(&mut conn, |tx| {
        // The `state` value being set is a bound `db::State` param (see below); the
        // `NOT IN ('done','cancelled')` guard stays a literal on purpose — multi-state
        // predicates don't parametrize idiomatically in rusqlite, and `db::State` remains
        // the single source of truth for what those literals may spell.
        // Read the origin before the transition, for the event payload only.
        // The guard names a set rather than one state, so the log cannot name
        // the origin without this; the `UPDATE` stays conditional and the
        // `changes() == 1` check below is still the sole authority on whether
        // the transition happened.
        let from = db::state_of(tx, task_id)?;
        let n = tx.execute(
            "UPDATE task SET state = ?1
              WHERE id = ?2 AND state NOT IN ('done','cancelled')",
            rusqlite::params![db::State::Cancelled, task_id],
        )?;
        if n != 1 {
            return Err(db::conflict(
                "already_terminal",
                format!("{} already terminal", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        // `completed_at IS NULL` in the WHERE is what makes closing the assignment
        // single-shot; the assignment is written plain because of it. This used to be
        // `COALESCE(completed_at, ?)`, whose first branch the same WHERE made
        // unreachable (QP-166) — and worse, it read as protection against clobbering an
        // already-closed assignment while `outcome = 'cancelled'` next to it had no such
        // guard. `abandon`, `reclaim`, `complete` and `block` all assign plain; this now
        // matches them.
        tx.execute(
            "UPDATE assignment SET outcome = 'cancelled', completed_at = ?
              WHERE task_id = ? AND completed_at IS NULL",
            rusqlite::params![crate::time::now_rfc3339(), task_id],
        )?;
        db::insert_state_change(
            tx,
            task_id,
            from.as_deref(),
            db::State::Cancelled.as_str(),
            None,
            &[("reason", serde_json::json!(a.reason))],
        )?;
        // Logged: `cancelled` counts as resolved, so cancelling a blocker
        // unblocks its dependents — a promotion that owes the log an entry.
        db::refresh_ready_logged(tx, None, "cancel")?;
        Ok(())
    })?;
    if let Some(r) = &release {
        r.finish(&conn);
    }
    emit(
        a.json,
        &Cancelled {
            display_id: resolved.display_id,
            state: "cancelled".to_string(),
            reason: a.reason,
        },
    )
}
