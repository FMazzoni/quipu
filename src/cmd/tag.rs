use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use crate::{db, id};

#[derive(Args, Debug)]
pub struct TagArgs {
    pub task: String,
    #[command(subcommand)] pub op: TagOp,
    /// Agent performing the tag operation (optional attribution).
    #[arg(long = "as")] pub agent: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum TagOp {
    Add { name: String },
    Rm  { name: String },
}

pub fn run(db_path: &std::path::Path, a: TagArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_id = id::resolve(&conn, &a.task)?;
    db::with_tx(&mut conn, |tx| {
        match &a.op {
            TagOp::Add { name } => {
                if name.is_empty() { return Err(anyhow!("tag name required")); }
                tx.execute("INSERT OR IGNORE INTO tag(task_id, name) VALUES (?,?)",
                    rusqlite::params![task_id, name])?;
                if tx.changes() == 1 {
                    db::insert_event(tx, Some(task_id), "tag_added", a.agent.as_deref(),
                        Some(&serde_json::json!({"name": name})))?;
                }
            }
            TagOp::Rm { name } => {
                tx.execute("DELETE FROM tag WHERE task_id = ? AND name = ?",
                    rusqlite::params![task_id, name])?;
                if tx.changes() == 1 {
                    db::insert_event(tx, Some(task_id), "tag_removed", a.agent.as_deref(),
                        Some(&serde_json::json!({"name": name})))?;
                }
            }
        }
        Ok(())
    })?;
    Ok(())
}
