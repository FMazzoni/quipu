use anyhow::Result;
use clap::Args;
use crate::{db, id};

#[derive(Args, Debug)]
pub struct BlockArgs {
    pub task: String,
    #[arg(long = "as")] pub agent: String,
    #[arg(long)] pub reason: String,
}

pub fn run(db_path: &std::path::Path, a: BlockArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_id = id::resolve(&conn, &a.task)?;
    db::with_tx(&mut conn, |tx| {
        let assignee: Option<String> = tx.query_row(
            "SELECT agent_id FROM assignment WHERE task_id = ? ORDER BY id DESC LIMIT 1",
            [task_id], |r| r.get(0)).ok();
        if assignee.as_deref() != Some(&a.agent) {
            return Err(db::constraint(format!("{} not yours", a.task)));
        }
        let n = tx.execute(
            "UPDATE task SET state = 'blocked' WHERE id = ? AND state IN ('assigned','running')",
            [task_id])?;
        if n != 1 { return Err(db::constraint(format!("{} not blockable from current state", a.task))); }
        db::insert_event(tx, Some(task_id), "state_change", Some(&a.agent),
            Some(&serde_json::json!({"to": "blocked", "reason": a.reason})))?;
        Ok(())
    })?;
    println!("{} blocked", a.task.to_uppercase());
    Ok(())
}
