//! `qp edit` — mutate task fields (title, tier, description). Emits one `edit` event.

use anyhow::Result;
use clap::Args;
use crate::{db, id};

#[derive(Args, Debug)]
pub struct EditArgs {
    pub task: String,
    #[arg(long)] pub title: Option<String>,
    #[arg(long)] pub tier: Option<String>,
    #[arg(long)] pub description: Option<String>,
    #[arg(long = "as")] pub agent: Option<String>,
}

pub fn run(db_path: &std::path::Path, a: EditArgs) -> Result<()> {
    if a.title.is_none() && a.tier.is_none() && a.description.is_none() {
        return Err(db::invalid_input(
            "qp edit requires at least one of --title, --tier, --description"));
    }
    let mut conn = db::open(db_path)?;
    let task_id = id::resolve(&conn, &a.task)?;
    let any_changed = db::with_tx(&mut conn, |tx| -> Result<bool> {
        let (cur_title, cur_tier, cur_description): (String, Option<String>, Option<String>) =
            tx.query_row(
                "SELECT title, tier, description FROM task WHERE id = ?",
                [task_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let mut changes = serde_json::Map::new();
        let mut sets: Vec<&str> = Vec::new();
        let mut params: Vec<rusqlite::types::Value> = Vec::new();

        if let Some(new_title) = &a.title {
            if *new_title != cur_title {
                changes.insert("title".into(),
                    serde_json::json!({"from": cur_title, "to": new_title}));
                sets.push("title = ?");
                params.push(new_title.clone().into());
            }
        }
        if let Some(new_tier) = &a.tier {
            let new_opt = if new_tier.is_empty() { None } else { Some(new_tier.clone()) };
            if new_opt != cur_tier {
                changes.insert("tier".into(),
                    serde_json::json!({"from": cur_tier, "to": new_opt}));
                sets.push("tier = ?");
                params.push(match &new_opt {
                    Some(s) => s.clone().into(),
                    None => rusqlite::types::Value::Null,
                });
            }
        }
        if let Some(new_desc) = &a.description {
            let new_opt = if new_desc.is_empty() { None } else { Some(new_desc.clone()) };
            if new_opt != cur_description {
                changes.insert("description".into(),
                    serde_json::json!({"from": cur_description, "to": new_opt}));
                sets.push("description = ?");
                params.push(match &new_opt {
                    Some(s) => s.clone().into(),
                    None => rusqlite::types::Value::Null,
                });
            }
        }

        if changes.is_empty() { return Ok(false); }

        let sql = format!("UPDATE task SET {} WHERE id = ?", sets.join(", "));
        params.push(task_id.into());
        let n = tx.execute(&sql, rusqlite::params_from_iter(params.iter()))?;
        if n != 1 {
            return Err(db::constraint(format!("{}: row vanished mid-edit", a.task)));
        }
        db::insert_event(tx, Some(task_id), "edit", a.agent.as_deref(),
            Some(&serde_json::json!({"changes": changes})))?;
        Ok(true)
    })?;
    if any_changed {
        println!("{} edited", a.task.to_uppercase());
    } else {
        println!("{} no changes", a.task.to_uppercase());
    }
    Ok(())
}
