//! The orchestrator-side release edge: `assigned`/`running` → `pending`.
//!
#![doc = include_str!("../../docs/modules/reclaim.md")]

use crate::outcome::{emit, Outcome};
use crate::{claimref, db, id};
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
    // Resolved before the transaction: the ref is deleted only once the
    // local release has committed. `None` on a local-only store, which is
    // what keeps this edge offline and unchanged.
    let release = claimref::release_for(db_path, &conn, task_id, &resolved.display_id)?;
    let resulting = db::with_tx(&mut conn, |tx| {
        // Origin read first, for the event payload only — the guard names a set,
        // so the log cannot tell `assigned` from `running` otherwise. The
        // `UPDATE` stays conditional and `changes() == 1` still decides.
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
        // The release and the re-promotion are two moves, and get two events —
        // see the note in `abandon`, which this mirrors.
        db::insert_state_change(
            tx,
            task_id,
            from.as_deref(),
            db::State::Pending.as_str(),
            None,
            &[
                ("via", serde_json::json!("reclaim")),
                ("reason", serde_json::json!(a.reason)),
            ],
        )?;
        let promoted = db::refresh_ready_logged(tx, None, "reclaim")?;
        let resulting = if promoted.contains(&task_id) {
            db::State::Ready.to_string()
        } else {
            db::State::Pending.to_string()
        };

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
        Ok(resulting)
    })?;
    if let Some(r) = &release {
        r.finish(&conn);
    }
    emit(
        a.json,
        &Reclaimed {
            display_id: resolved.display_id,
            state: resulting,
            reason: a.reason,
        },
    )
}
