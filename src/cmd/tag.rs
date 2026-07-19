use crate::outcome::{emit, Outcome};
use crate::{db, id};
use anyhow::Result;
use clap::{Args, Subcommand};
use serde::Serialize;

#[derive(Args, Debug)]
pub struct TagArgs {
    pub task: String,
    #[command(subcommand)]
    pub op: TagOp,
    /// Agent performing the tag operation (optional attribution).
    #[arg(long = "as")]
    pub agent: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum TagOp {
    Add { name: String },
    Rm { name: String },
}

#[derive(Serialize)]
struct Tagged {
    display_id: String,
    op: &'static str,
    name: String,
}
impl Outcome for Tagged {
    fn human(&self) -> String {
        match self.op {
            "added" => format!("{} tagged {}", self.display_id, self.name),
            "removed" => format!("{} untagged {}", self.display_id, self.name),
            _ => format!("{} already {}", self.display_id, self.name),
        }
    }
}

pub fn run(db_path: &std::path::Path, a: TagArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    let (op, name) = db::with_tx(&mut conn, |tx| -> Result<(&'static str, String)> {
        match &a.op {
            TagOp::Add { name } => {
                if name.is_empty() {
                    return Err(db::invalid_input("tag name required"));
                }
                tx.execute(
                    "INSERT OR IGNORE INTO tag(task_id, name) VALUES (?,?)",
                    rusqlite::params![task_id, name],
                )?;
                let op = if tx.changes() == 1 {
                    db::insert_event(
                        tx,
                        Some(task_id),
                        "tag_added",
                        a.agent.as_deref(),
                        Some(&serde_json::json!({"name": name})),
                    )?;
                    "added"
                } else {
                    "noop"
                };
                Ok((op, name.clone()))
            }
            TagOp::Rm { name } => {
                tx.execute(
                    "DELETE FROM tag WHERE task_id = ? AND name = ?",
                    rusqlite::params![task_id, name],
                )?;
                let op = if tx.changes() == 1 {
                    db::insert_event(
                        tx,
                        Some(task_id),
                        "tag_removed",
                        a.agent.as_deref(),
                        Some(&serde_json::json!({"name": name})),
                    )?;
                    "removed"
                } else {
                    "noop"
                };
                Ok((op, name.clone()))
            }
        }
    })?;
    emit(
        a.json,
        &Tagged {
            display_id: resolved.display_id,
            op,
            name,
        },
    )
}
