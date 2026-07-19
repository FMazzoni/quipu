//! `qp edit` — mutate task fields (title, tier, description).
//!
//! Emits one `edit` event when a field actually changes; a no-op edit emits nothing.

use crate::outcome::{emit, Outcome};
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct EditArgs {
    pub task: String,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub tier: Option<String>,
    #[arg(long)]
    pub description: Option<String>,
    #[arg(long = "as")]
    pub agent: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct Edited {
    display_id: String,
    changed: bool,
    changes: serde_json::Value,
}
impl Outcome for Edited {
    fn human(&self) -> String {
        if self.changed {
            format!("{} edited", self.display_id)
        } else {
            format!("{} no changes", self.display_id)
        }
    }
}

pub fn run(db_path: &std::path::Path, a: EditArgs) -> Result<()> {
    if a.title.is_none() && a.tier.is_none() && a.description.is_none() {
        return Err(db::invalid_input(
            "qp edit requires at least one of --title, --tier, --description",
        ));
    }
    if let Some(t) = &a.title {
        if t.is_empty() {
            return Err(db::invalid_input("--title cannot be empty"));
        }
    }
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    let (any_changed, changes) =
        db::with_tx(&mut conn, |tx| -> Result<(bool, serde_json::Value)> {
            let (cur_title, cur_tier, cur_description): (String, Option<String>, Option<String>) =
                tx.query_row(
                    "SELECT title, tier, description FROM task WHERE id = ?",
                    [task_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
            let mut changes = serde_json::Map::new();
            let mut sets: Vec<&str> = Vec::new();
            let mut params: Vec<rusqlite::types::Value> = Vec::new();

            if let Some(new_title) = &a.title {
                if *new_title != cur_title {
                    changes.insert(
                        "title".into(),
                        serde_json::json!({"from": cur_title, "to": new_title}),
                    );
                    sets.push("title = ?");
                    params.push(new_title.clone().into());
                }
            }
            if let Some(new_tier) = &a.tier {
                let new_opt = if new_tier.is_empty() {
                    None
                } else {
                    Some(new_tier.clone())
                };
                if new_opt != cur_tier {
                    changes.insert(
                        "tier".into(),
                        serde_json::json!({"from": cur_tier, "to": new_opt}),
                    );
                    sets.push("tier = ?");
                    params.push(match &new_opt {
                        Some(s) => s.clone().into(),
                        None => rusqlite::types::Value::Null,
                    });
                }
            }
            if let Some(new_desc) = &a.description {
                let new_opt = if new_desc.is_empty() {
                    None
                } else {
                    Some(new_desc.clone())
                };
                if new_opt != cur_description {
                    changes.insert(
                        "description".into(),
                        serde_json::json!({"from": cur_description, "to": new_opt}),
                    );
                    sets.push("description = ?");
                    params.push(match &new_opt {
                        Some(s) => s.clone().into(),
                        None => rusqlite::types::Value::Null,
                    });
                }
            }

            if changes.is_empty() {
                return Ok((false, serde_json::Value::Object(changes)));
            }

            let sql = format!(
                "UPDATE task SET {} WHERE id = ? AND state NOT IN ('done','cancelled')",
                sets.join(", ")
            );
            params.push(task_id.into());
            let n = tx.execute(&sql, rusqlite::params_from_iter(params.iter()))?;
            if n != 1 {
                return Err(db::conflict(
                    "not_editable",
                    format!("{}: not editable (terminal state or vanished)", a.task),
                    Some(resolved.display_id.clone()),
                ));
            }
            db::insert_event(
                tx,
                Some(task_id),
                "edit",
                a.agent.as_deref(),
                Some(&serde_json::json!({"changes": changes})),
            )?;
            Ok((true, serde_json::Value::Object(changes)))
        })?;
    emit(
        a.json,
        &Edited {
            display_id: resolved.display_id,
            changed: any_changed,
            changes,
        },
    )
}
