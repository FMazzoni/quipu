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
        // Latest open assignment must be (a) for this agent (b) un-claimed.
        let Some(open) = db::current_assignment(tx, task_id)? else {
            return Err(db::constraint(format!("{} has no assignment", a.task)));
        };
        let aid = open.id;
        if open.claimed_at.is_some() {
            return Err(db::constraint(format!("{} already claimed", a.task)));
        }
        if open.agent_id != a.agent {
            return Err(db::constraint(format!(
                "{} assigned to `{}`, not `{}`",
                a.task, open.agent_id, a.agent
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
