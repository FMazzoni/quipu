use crate::{db, id};
use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct AbandonArgs {
    pub task: String,
    #[arg(long = "as")]
    pub agent: String,
    #[arg(long)]
    pub reason: Option<String>,
}

pub fn run(db_path: &std::path::Path, a: AbandonArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_id = id::resolve(&conn, &a.task)?;
    db::with_tx(&mut conn, |tx| {
        let Some(open) = db::current_assignment(tx, task_id)? else {
            return Err(db::constraint(format!("{} has no assignment", a.task)));
        };
        let aid = open.id;
        if open.agent_id != a.agent {
            return Err(db::constraint(format!("{} not yours", a.task)));
        }

        // Route through `pending`, then let `refresh_ready` promote it back to `ready`
        // if it has no unresolved deps. Same destination logic as `reclaim`, one code path.
        let n = tx.execute(
            "UPDATE task SET state = 'pending' WHERE id = ?1 AND state IN ('assigned','running')",
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
            "UPDATE assignment SET completed_at = ?, outcome = 'abandoned' \
              WHERE id = ? AND completed_at IS NULL",
            rusqlite::params![crate::time::now_rfc3339(), aid],
        )?;
        if n2 != 1 {
            return Err(db::constraint(format!(
                "{} assignment already closed",
                a.task
            )));
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
        Ok(())
    })?;
    println!("{} abandoned", a.task.to_uppercase());
    Ok(())
}
