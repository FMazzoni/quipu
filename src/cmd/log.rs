use anyhow::Result;
use clap::Args;
use crate::{db, id};

#[derive(Args, Debug)]
pub struct LogArgs {
    pub task: String,
    pub kind: String,
    pub body: String,
    #[arg(long = "as")] pub agent: Option<String>,
    #[arg(long)] pub auto: bool,
}

pub fn run(db_path: &std::path::Path, a: LogArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_id = id::resolve(&conn, &a.task)?;
    db::with_tx(&mut conn, |tx| {
        let mut payload = serde_json::json!({"text": a.body});
        if a.auto { payload["auto"] = serde_json::Value::Bool(true); }
        db::insert_event(tx, Some(task_id), &a.kind, a.agent.as_deref(), Some(&payload))?;
        Ok(())
    })?;
    println!("logged {} on {}", a.kind, a.task.to_uppercase());
    Ok(())
}
