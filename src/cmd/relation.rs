//! Manage typed, non-blocking references between tasks.
//!
#![doc = include_str!("../../docs/modules/relation.md")]

use crate::outcome::{emit, Outcome};
use crate::{db, id};
use anyhow::Result;
use clap::{Args, Subcommand};
use serde::Serialize;

#[derive(Args, Debug)]
pub struct RelationArgs {
    #[command(subcommand)]
    pub op: RelOp,
}

#[derive(Subcommand, Debug)]
pub enum RelOp {
    Add {
        from: String,
        kind: String,
        to: String,
        #[arg(long)]
        json: bool,
    },
    Rm {
        from: String,
        kind: String,
        to: String,
        #[arg(long)]
        json: bool,
    },
    List {
        task: String,
        #[arg(long)]
        json: bool,
    },
}

impl RelationArgs {
    /// Whether `--json` was passed on whichever subcommand was chosen.
    ///
    /// Lets `main`'s error path match the stream format the success path uses.
    pub fn json(&self) -> bool {
        match &self.op {
            RelOp::Add { json, .. } | RelOp::Rm { json, .. } | RelOp::List { json, .. } => *json,
        }
    }
}

#[derive(Serialize)]
struct RelationAdded {
    from_display_id: String,
    to_display_id: String,
    kind: String,
    added: bool,
}
impl Outcome for RelationAdded {
    fn human(&self) -> String {
        if self.added {
            format!(
                "{} {} {} linked",
                self.from_display_id, self.kind, self.to_display_id
            )
        } else {
            format!(
                "{} {} {} already linked",
                self.from_display_id, self.kind, self.to_display_id
            )
        }
    }
}

#[derive(Serialize)]
struct RelationRemoved {
    from_display_id: String,
    to_display_id: String,
    kind: String,
    removed: bool,
}
impl Outcome for RelationRemoved {
    fn human(&self) -> String {
        if self.removed {
            format!(
                "{} {} {} unlinked",
                self.from_display_id, self.kind, self.to_display_id
            )
        } else {
            format!(
                "{} {} {} not linked",
                self.from_display_id, self.kind, self.to_display_id
            )
        }
    }
}

pub fn run(db_path: &std::path::Path, a: RelationArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    match a.op {
        RelOp::Add {
            from,
            kind,
            to,
            json,
        } => {
            if kind.is_empty() {
                return Err(db::invalid_input("relation kind required"));
            }
            let f = id::resolve_full(&conn, &from)?;
            let t = id::resolve_full(&conn, &to)?;
            let added = db::with_tx(&mut conn, |tx| -> Result<bool> {
                tx.execute(
                    "INSERT OR IGNORE INTO relation(from_task_id, to_task_id, kind) VALUES (?,?,?)",
                    rusqlite::params![f.id, t.id, kind],
                )?;
                if tx.changes() == 1 {
                    db::insert_event(
                        tx,
                        Some(f.id),
                        "relation_add",
                        None,
                        Some(&serde_json::json!({"kind": kind, "to": t.display_id})),
                    )?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            })?;
            emit(
                json,
                &RelationAdded {
                    from_display_id: f.display_id,
                    to_display_id: t.display_id,
                    kind,
                    added,
                },
            )?;
        }
        RelOp::Rm {
            from,
            kind,
            to,
            json,
        } => {
            let f = id::resolve_full(&conn, &from)?;
            let t = id::resolve_full(&conn, &to)?;
            let removed = db::with_tx(&mut conn, |tx| -> Result<bool> {
                tx.execute(
                    "DELETE FROM relation WHERE from_task_id = ? AND to_task_id = ? AND kind = ?",
                    rusqlite::params![f.id, t.id, kind],
                )?;
                if tx.changes() > 0 {
                    db::insert_event(
                        tx,
                        Some(f.id),
                        "relation_removed",
                        None,
                        Some(&serde_json::json!({"kind": kind, "to": t.display_id})),
                    )?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            })?;
            emit(
                json,
                &RelationRemoved {
                    from_display_id: f.display_id,
                    to_display_id: t.display_id,
                    kind,
                    removed,
                },
            )?;
        }
        RelOp::List { task, json } => {
            let task_id = id::resolve(&conn, &task)?;
            let outgoing: Vec<serde_json::Value> = {
                let mut s = conn.prepare(
                    "SELECT t.display_id, r.kind FROM relation r
                       JOIN task t ON t.id = r.to_task_id
                      WHERE r.from_task_id = ? ORDER BY r.kind, t.id",
                )?;
                let x = s
                    .query_map([task_id], |r| {
                        Ok(serde_json::json!({
                            "to": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?
                        }))
                    })?
                    .collect::<Result<_, _>>()?;
                x
            };
            let incoming: Vec<serde_json::Value> = {
                let mut s = conn.prepare(
                    "SELECT t.display_id, r.kind FROM relation r
                       JOIN task t ON t.id = r.from_task_id
                      WHERE r.to_task_id = ? ORDER BY r.kind, t.id",
                )?;
                let x = s
                    .query_map([task_id], |r| {
                        Ok(serde_json::json!({
                            "from": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?
                        }))
                    })?
                    .collect::<Result<_, _>>()?;
                x
            };
            let bundle = serde_json::json!({"outgoing": outgoing, "incoming": incoming});
            if json {
                println!("{}", serde_json::to_string(&bundle)?);
            } else {
                for o in &outgoing {
                    println!(
                        "→ {} {}",
                        o["kind"].as_str().unwrap(),
                        o["to"].as_str().unwrap()
                    );
                }
                for i in &incoming {
                    println!(
                        "← {} {}",
                        i["kind"].as_str().unwrap(),
                        i["from"].as_str().unwrap()
                    );
                }
            }
        }
    }
    Ok(())
}
