//! The orchestrator-side release edge: `assigned`/`running` → `pending`.
//!
//! Force-release with no ownership check, for when an agent has died and
//! cannot release its own claim. Compare `abandon`.

use crate::outcome::{emit, Outcome};
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct ReclaimArgs {
    pub task: String,
    #[arg(long)]
    pub reason: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct Reclaimed {
    display_id: String,
    state: String,
    reason: Option<String>,
}
impl Outcome for Reclaimed {
    fn human(&self) -> String {
        format!("{} reclaimed", self.display_id)
    }
}

pub fn run(db_path: &std::path::Path, a: ReclaimArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    let resulting = db::with_tx(&mut conn, |tx| {
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
            "UPDATE assignment SET completed_at = ?, outcome = 'reclaimed'
              WHERE task_id = ? AND completed_at IS NULL",
            rusqlite::params![crate::time::now_rfc3339(), task_id],
        )?;
        if n2 == 0 {
            return Err(db::conflict(
                "no_open_assignment",
                format!("{} has no open assignment rows to close", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            None,
            Some(&serde_json::json!({"to": resulting, "via": "reclaim", "reason": a.reason})),
        )?;
        Ok(resulting)
    })?;
    emit(
        a.json,
        &Reclaimed {
            display_id: resolved.display_id,
            state: resulting,
            reason: a.reason,
        },
    )
}
