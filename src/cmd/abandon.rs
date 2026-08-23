//! The agent-side release edge: `assigned`/`running` → `pending`.
//!
#![doc = include_str!("../../docs/modules/abandon.md")]

use crate::outcome::{emit, Outcome};
use crate::{claimref, db, id};
use anyhow::Result;
use clap::Args;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct AbandonArgs {
    pub task: String,
    #[arg(long = "as")]
    pub agent: String,
    #[arg(long)]
    pub reason: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct Abandoned {
    display_id: String,
    state: String,
    reason: Option<String>,
}
impl Outcome for Abandoned {
    fn human(&self) -> String {
        format!("{} abandoned", self.display_id)
    }
}

pub fn run(db_path: &std::path::Path, a: AbandonArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    // Resolved before the transaction: the ref is deleted only once the
    // local release has committed. `None` on a local-only store, which is
    // what keeps this edge offline and unchanged.
    let release = claimref::release_for(db_path, &conn, task_id, &a.agent)?;
    let resulting = db::with_tx(&mut conn, |tx| {
        let Some(open) = db::current_assignment(tx, task_id)? else {
            return Err(db::conflict(
                "no_open_assignment",
                format!("{} has no assignment", a.task),
                Some(resolved.display_id.clone()),
            ));
        };
        let aid = open.id;
        if open.agent_id != a.agent {
            return Err(db::not_owner(
                format!("{} not yours", a.task),
                Some(resolved.display_id.clone()),
                Some(open.agent_id.clone()),
            ));
        }

        // Route through `pending`, then let the readiness sweep promote it back to
        // `ready` if it has no unresolved deps. Same destination logic as
        // `reclaim`, one code path.
        //
        // The origin is read first, for the event payload only: the guard names
        // a set, so `assigned` and `running` are both legal here and the log
        // cannot tell them apart otherwise. The `UPDATE` stays conditional.
        let from = db::state_of(tx, task_id)?;
        let n = tx.execute(
            "UPDATE task SET state = ?1 WHERE id = ?2 AND state IN ('assigned','running')",
            rusqlite::params![db::State::Pending, task_id],
        )?;
        if n != 1 {
            return Err(db::conflict(
                "not_assigned_or_running",
                format!("{} not assigned/running", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        // Two events, not one. This used to emit a single `state_change` naming
        // the state the task landed in after the sweep, which reads as a direct
        // `running` → `ready` hop — a transition the state machine does not
        // have. The release and the re-promotion are separate moves and the log
        // now says so, which is what makes the sequence replayable.
        db::insert_state_change(
            tx,
            task_id,
            from.as_deref(),
            db::State::Pending.as_str(),
            Some(&a.agent),
            &[
                ("via", serde_json::json!("abandon")),
                ("reason", serde_json::json!(a.reason)),
            ],
        )?;
        let promoted = db::refresh_ready_logged(tx, Some(&a.agent), "abandon")?;
        let resulting = if promoted.contains(&task_id) {
            db::State::Ready.to_string()
        } else {
            db::State::Pending.to_string()
        };

        let n2 = tx.execute(
            "UPDATE assignment SET completed_at = ?, outcome = 'abandoned' \
              WHERE id = ? AND completed_at IS NULL",
            rusqlite::params![crate::time::now_rfc3339(), aid],
        )?;
        if n2 != 1 {
            return Err(db::conflict(
                "already_closed",
                format!("{} assignment already closed", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        Ok(resulting)
    })?;
    if let Some(r) = &release {
        r.finish(&conn);
    }
    emit(
        a.json,
        &Abandoned {
            display_id: resolved.display_id,
            state: resulting,
            reason: a.reason,
        },
    )
}
