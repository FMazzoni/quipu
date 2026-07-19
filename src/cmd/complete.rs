use crate::{db, id};
use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct CompleteArgs {
    pub task: String,
    #[arg(long = "as")]
    pub agent: String,
    #[arg(long, value_name = "TEXT")]
    pub decision: Vec<String>,
    #[arg(long = "artifact", value_name = "PATH")]
    pub artifact: Vec<String>,
}

pub fn run(db_path: &std::path::Path, a: CompleteArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    db::with_tx(&mut conn, |tx| {
        let Some(open) = db::current_assignment(tx, task_id)? else {
            return Err(db::constraint(format!("{} not assigned", a.task)));
        };
        let aid = open.id;
        if open.claimed_at.is_none() {
            return Err(db::constraint(format!("{} not claimed", a.task)));
        }
        if open.agent_id != a.agent {
            return Err(db::constraint(format!("{} not yours", a.task)));
        }
        let n = tx.execute(
            "UPDATE task SET state = 'done' WHERE id = ? AND state = 'running'",
            [task_id],
        )?;
        if n != 1 {
            return Err(db::constraint(format!("{} not in running state", a.task)));
        }
        tx.execute(
            "UPDATE assignment SET completed_at = ?, outcome = 'success' WHERE id = ?",
            rusqlite::params![crate::time::now_rfc3339(), aid],
        )?;
        for d in &a.decision {
            db::insert_event(
                tx,
                Some(task_id),
                "decision",
                Some(&a.agent),
                Some(&serde_json::json!({"text": d})),
            )?;
        }
        for p in &a.artifact {
            db::insert_event(
                tx,
                Some(task_id),
                "artifact",
                Some(&a.agent),
                Some(&serde_json::json!({"path": p})),
            )?;
        }
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            Some(&a.agent),
            Some(&serde_json::json!({"to": "done"})),
        )?;
        db::refresh_ready(tx)?;
        Ok(())
    })?;
    println!("{} done", resolved.display_id);
    Ok(())
}
