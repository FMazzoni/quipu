use crate::outcome::{emit, Outcome};
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct AssignArgs {
    pub task: String,
    #[arg(long = "to")]
    pub to: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct Assigned {
    display_id: String,
    agent_id: String,
    state: String,
}
impl Outcome for Assigned {
    fn human(&self) -> String {
        format!("{} assigned to {}", self.display_id, self.agent_id)
    }
}

pub fn run(db_path: &std::path::Path, a: AssignArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    db::with_tx(&mut conn, |tx| {
        let n = tx.execute(
            "UPDATE task SET state = 'assigned' WHERE id = ? AND state = 'ready'",
            [task_id],
        )?;
        if n != 1 {
            return Err(db::conflict(
                "not_ready",
                format!("{} not ready for assignment", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        let n = tx.execute(
            "INSERT INTO assignment(task_id, agent_id)
             SELECT ?1, ?2 WHERE NOT EXISTS (
               SELECT 1 FROM assignment WHERE task_id = ?1 AND completed_at IS NULL)",
            rusqlite::params![task_id, a.to],
        )?;
        if n != 1 {
            return Err(db::conflict(
                "stale_open_assignment",
                format!("{} has a stale open assignment", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            Some(&a.to),
            Some(&serde_json::json!({"to": "assigned", "agent_id": a.to})),
        )?;
        Ok(())
    })?;
    emit(
        a.json,
        &Assigned {
            display_id: resolved.display_id,
            agent_id: a.to,
            state: "assigned".to_string(),
        },
    )
}
