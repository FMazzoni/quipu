use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use crate::{db, id};

#[derive(Args, Debug)]
pub struct TagArgs {
    pub task: String,
    #[command(subcommand)] pub op: TagOp,
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
            }
            TagOp::Rm { name } => {
                tx.execute("DELETE FROM tag WHERE task_id = ? AND name = ?",
                    rusqlite::params![task_id, name])?;
            }
        }
        Ok(())
    })?;
    Ok(())
}
