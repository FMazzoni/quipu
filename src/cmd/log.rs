use crate::{db, id};
use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct LogArgs {
    pub task: String,
    pub kind: String,
    pub body: String,
    #[arg(long = "as")]
    pub agent: Option<String>,
    #[arg(long)]
    pub auto: bool,
}

pub fn run(db_path: &std::path::Path, a: LogArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    db::with_tx(&mut conn, |tx| {
        let mut payload = serde_json::json!({"text": a.body});
        if a.auto {
            payload["auto"] = serde_json::Value::Bool(true);
        }
        // If --as wasn't provided, auto-attribute to the latest open assignee
        // iff the task is currently running (unambiguous owner).
        let auto_agent: Option<String> = if a.agent.is_none() {
            let state: Option<String> = tx
                .query_row("SELECT state FROM task WHERE id = ?", [task_id], |r| {
                    r.get(0)
                })
                .ok();
            if state.as_deref() == Some("running") {
                tx.query_row(
                    "SELECT agent_id FROM assignment
                      WHERE task_id = ? AND completed_at IS NULL
                      ORDER BY id DESC LIMIT 1",
                    [task_id],
                    |r| r.get(0),
                )
                .ok()
            } else {
                None
            }
        } else {
            None
        };
        let agent = a.agent.as_deref().or(auto_agent.as_deref());
        db::insert_event(tx, Some(task_id), &a.kind, agent, Some(&payload))?;
        Ok(())
    })?;
    println!("logged {} on {}", a.kind, resolved.display_id);
    Ok(())
}
