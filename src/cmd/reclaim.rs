use crate::{db, id};
use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct ReclaimArgs {
    pub task: String,
    #[arg(long)]
    pub reason: Option<String>,
}

pub fn run(db_path: &std::path::Path, a: ReclaimArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    db::with_tx(&mut conn, |tx| {
        let n = tx.execute(
            "UPDATE task SET state = 'pending' WHERE id = ? AND state IN ('assigned','running')",
            [task_id],
        )?;
        if n != 1 {
            return Err(db::constraint(format!("{} not assigned/running", a.task)));
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
            return Err(db::constraint(format!(
                "{} has no open assignment rows to close",
                a.task
            )));
        }
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            None,
            Some(&serde_json::json!({"to": resulting, "via": "reclaim", "reason": a.reason})),
        )?;
        Ok(())
    })?;
    println!("{} reclaimed", resolved.display_id);
    Ok(())
}
