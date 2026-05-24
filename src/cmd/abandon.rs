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
        if assignee != a.agent { return Err(db::constraint(format!("{} not yours", a.task))); }
        let n = tx.execute(
            "UPDATE task SET state = 'ready' WHERE id = ? AND state IN ('assigned','running')",
            [task_id])?;
        if n != 1 { return Err(db::constraint(format!("{} not assigned/running", a.task))); }
        tx.execute(
            "UPDATE assignment SET completed_at = ?, outcome = 'abandoned' WHERE id = ?",
            rusqlite::params![crate::time::now_rfc3339(), aid])?;
        db::insert_event(tx, Some(task_id), "state_change", Some(&a.agent),
            Some(&serde_json::json!({"to": "ready", "via": "abandon", "reason": a.reason})))?;
        Ok(())
    })?;
    println!("{} abandoned", a.task.to_uppercase());
    Ok(())
}
