//! Add or remove flat labels on a task.
//!
#![doc = include_str!("../../docs/modules/tag.md")]

use crate::outcome::Outcome;
use crate::{db, id};
use anyhow::Result;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagArgs {
    pub task: String,
    #[command(subcommand)]
    pub op: TagOp,
    /// Agent performing the tag operation (optional attribution).
    #[arg(long = "as")]
    #[serde(default)]
    pub agent: Option<String>,
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
}

/// The chosen op, externally tagged so the body reads `{"op":{"add":{...}}}`.
///
/// Lower-cased to match what the caller types: a hand-written request should
/// spell the subcommand the way the CLI does, not the way Rust does.
#[derive(Subcommand, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum TagOp {
    Add { name: String },
    Rm { name: String },
}

impl crate::route::Served for TagArgs {
    const VERB: &'static str = "tag";
    type Out = Tagged;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct Tagged {
    display_id: String,
    op: String,
    name: String,
}
impl Outcome for Tagged {
    fn human(&self) -> String {
        match self.op.as_str() {
            "added" => format!("{} tagged {}", self.display_id, self.name),
            "removed" => format!("{} untagged {}", self.display_id, self.name),
            _ => format!("{} already {}", self.display_id, self.name),
        }
    }
}

pub fn exec(db_path: &std::path::Path, a: TagArgs) -> Result<Tagged> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    let (op, name) = db::with_tx(&mut conn, |tx| -> Result<(&'static str, String)> {
        match &a.op {
            TagOp::Add { name } => {
                if name.is_empty() {
                    return Err(db::invalid_input("tag name required"));
                }
                if name.ends_with(':') {
                    return Err(db::invalid_input(format!(
                        "tag '{name}' has an empty value after ':' \
                         (a shell substitution that produced nothing?); \
                         use a bare name without the colon for an unnamespaced tag"
                    )));
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
    Ok(Tagged {
        display_id: resolved.display_id,
        op: op.to_string(),
        name,
    })
}
