use crate::{db, id};
use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct ClaimArgs {
    pub task: String,
    #[arg(long = "as")]
    pub agent: String,
}

pub fn run(db_path: &std::path::Path, a: ClaimArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_id = id::resolve(&conn, &a.task)?;
    db::with_tx(&mut conn, |tx| {
        // Latest assignment must be (a) for this agent (b) un-claimed (c) un-completed.
        let row: Option<(i64, String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT id, agent_id, claimed_at, completed_at FROM assignment
              WHERE task_id = ? ORDER BY id DESC LIMIT 1",
                [task_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok();
        let Some((aid, assignee, claimed, completed)) = row else {
            return Err(db::constraint(format!("{} has no assignment", a.task)));
        };
        if completed.is_some() {
            return Err(db::constraint(format!(
                "{} assignment already completed",
                a.task
            )));
        }
        if claimed.is_some() {
            return Err(db::constraint(format!("{} already claimed", a.task)));
        }
        if assignee != a.agent {
            return Err(db::constraint(format!(
                "{} assigned to `{assignee}`, not `{}`",
                a.task, a.agent
            )));
        }
        // Guarded transition: task must currently be `assigned`.
        let n = tx.execute(
            "UPDATE task SET state = 'running' WHERE id = ? AND state = 'assigned'",
            [task_id],
        )?;
        if n != 1 {
            return Err(db::constraint(format!("{} state changed under us", a.task)));
        }
        tx.execute(
            "UPDATE assignment SET claimed_at = ? WHERE id = ?",
            rusqlite::params![crate::time::now_rfc3339(), aid],
        )?;
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            Some(&a.agent),
            Some(&serde_json::json!({"to": "running"})),
        )?;
        Ok(())
    })?;
    println!("{} claimed by {}", a.task.to_uppercase(), a.agent);
    Ok(())
}
