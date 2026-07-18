use crate::{db, id};
use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct AssignArgs {
    pub task: String,
    #[arg(long = "to")]
    pub to: String,
}

pub fn run(db_path: &std::path::Path, a: AssignArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_id = id::resolve(&conn, &a.task)?;
    db::with_tx(&mut conn, |tx| {
        let n = tx.execute(
            "UPDATE task SET state = 'assigned' WHERE id = ? AND state = 'ready'",
            [task_id],
        )?;
        if n != 1 {
            return Err(db::constraint(format!(
                "{} not ready for assignment",
                a.task
            )));
        }
        tx.execute(
            "INSERT INTO assignment(task_id, agent_id) VALUES (?,?)",
            rusqlite::params![task_id, a.to],
        )?;
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            Some(&a.to),
            Some(&serde_json::json!({"to": "assigned", "agent_id": a.to})),
        )?;
        Ok(())
    })?;
    println!("{} assigned to {}", a.task.to_uppercase(), a.to);
    Ok(())
}
