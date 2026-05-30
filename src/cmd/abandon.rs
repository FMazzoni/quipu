use anyhow::Result;
use clap::Args;
use crate::{db, id};

#[derive(Args, Debug)]
pub struct AbandonArgs {
    pub task: String,
    #[arg(long = "as")] pub agent: String,
    #[arg(long)] pub reason: Option<String>,
}

pub fn run(db_path: &std::path::Path, a: AbandonArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_id = id::resolve(&conn, &a.task)?;
    db::with_tx(&mut conn, |tx| {
        let assignment: Option<(i64, String)> = tx.query_row(
            "SELECT id, agent_id FROM assignment WHERE task_id = ? ORDER BY id DESC LIMIT 1",
            [task_id], |r| Ok((r.get(0)?, r.get(1)?))
        ).ok();
        let Some((aid, assignee)) = assignment else {
            return Err(db::constraint(format!("{} has no assignment", a.task)));
        };
        if assignee != a.agent {
            return Err(db::constraint(format!("{} not yours", a.task)));
        }

        // Single guarded UPDATE: destination is `pending` if any unresolved dep exists,
        // else `ready`. Matches the post-MVP state machine (no `blocked`).
        let n = tx.execute(
            "UPDATE task
                SET state = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM dep d
                        JOIN task t2 ON t2.id = d.depends_on_task_id
                        WHERE d.task_id = task.id
                          AND t2.state NOT IN ('done','cancelled')
                    ) THEN 'pending'
                    ELSE 'ready'
                END
              WHERE id = ?1 AND state IN ('assigned','running')",
            [task_id])?;
        if n != 1 {
            return Err(db::constraint(format!("{} not assigned/running", a.task)));
        }

        // Read back the resulting state for the event payload. Permitted exception:
        // auxiliary read for error/event-quality, not control flow.
        let resulting: String = tx.query_row(
            "SELECT state FROM task WHERE id = ?", [task_id], |r| r.get(0))?;

        let n2 = tx.execute(
            "UPDATE assignment SET completed_at = ?, outcome = 'abandoned' \
              WHERE id = ? AND completed_at IS NULL",
            rusqlite::params![crate::time::now_rfc3339(), aid])?;
        if n2 != 1 {
            return Err(db::constraint(format!("{} assignment already closed", a.task)));
        }
        db::insert_event(tx, Some(task_id), "state_change", Some(&a.agent),
            Some(&serde_json::json!({
                "to": resulting, "via": "abandon", "reason": a.reason
            })))?;
        Ok(())
    })?;
    println!("{} abandoned", a.task.to_uppercase());
    Ok(())
}
