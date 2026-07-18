use crate::{db, id};
use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct CancelArgs {
    pub task: String,
    #[arg(long)]
    pub reason: Option<String>,
}

pub fn run(db_path: &std::path::Path, a: CancelArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_id = id::resolve(&conn, &a.task)?;
    db::with_tx(&mut conn, |tx| {
        let n = tx.execute(
            "UPDATE task SET state = 'cancelled'
              WHERE id = ? AND state NOT IN ('done','cancelled')",
            [task_id],
        )?;
        if n != 1 {
            return Err(db::constraint(format!("{} already terminal", a.task)));
        }
        tx.execute(
            "UPDATE assignment SET outcome = 'cancelled', completed_at = COALESCE(completed_at, ?)
              WHERE task_id = ? AND completed_at IS NULL",
            rusqlite::params![crate::time::now_rfc3339(), task_id],
        )?;
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            None,
            Some(&serde_json::json!({"to": "cancelled", "reason": a.reason})),
        )?;
        db::refresh_ready(tx)?;
        Ok(())
    })?;
    println!("{} cancelled", a.task.to_uppercase());
    Ok(())
}
