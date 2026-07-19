//! The agent-side release edge: `assigned`/`running` → `pending`.
//!
//! Ownership-checked: an agent may only release its own claim. Returns the
//! task to `pending` rather than guessing whether its deps still hold;
//! `refresh_ready` promotes it when they do. Compare `reclaim`.

use crate::outcome::{emit, Outcome};
use crate::{db, id};
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

        // Route through `pending`, then let `refresh_ready` promote it back to `ready`
        // if it has no unresolved deps. Same destination logic as `reclaim`, one code path.
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
        db::refresh_ready(tx)?;

        // Read back the resulting state for the event payload. Permitted exception:
        // auxiliary read for error/event-quality, not control flow.
        let resulting: String =
            tx.query_row("SELECT state FROM task WHERE id = ?", [task_id], |r| {
                r.get(0)
            })?;

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
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            Some(&a.agent),
            Some(&serde_json::json!({
                "to": resulting, "via": "abandon", "reason": a.reason
            })),
        )?;
        Ok(resulting)
    })?;
    emit(
        a.json,
        &Abandoned {
            display_id: resolved.display_id,
            state: resulting,
            reason: a.reason,
        },
    )
}
