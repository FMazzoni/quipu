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
    let task_id = id::resolve(&conn, &a.task)?;
    db::with_tx(&mut conn, |tx| {
        let n = tx.execute(
            "UPDATE task SET state = 'ready' WHERE id = ? AND state IN ('assigned','running')",
            [task_id],
        )?;
        if n != 1 {
            return Err(db::constraint(format!("{} not assigned/running", a.task)));
        }
        tx.execute(
            "UPDATE assignment SET completed_at = ?, outcome = 'reclaimed'
              WHERE task_id = ? AND completed_at IS NULL",
            rusqlite::params![crate::time::now_rfc3339(), task_id],
        )?;
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            None,
            Some(&serde_json::json!({"to": "ready", "via": "reclaim", "reason": a.reason})),
        )?;
        Ok(())
    })?;
    println!("{} reclaimed", a.task.to_uppercase());
    Ok(())
}
