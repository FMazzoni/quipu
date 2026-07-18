use crate::{db, id};
use anyhow::Result;
use clap::{Args, Subcommand};

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
    },
    Rm {
        from: String,
        kind: String,
        to: String,
    },
    List {
        task: String,
        #[arg(long)]
        json: bool,
    },
}

pub fn run(db_path: &std::path::Path, a: RelationArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    match a.op {
        RelOp::Add { from, kind, to } => {
            let f = id::resolve(&conn, &from)?;
            let t = id::resolve(&conn, &to)?;
            db::with_tx(&mut conn, |tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO relation(from_task_id, to_task_id, kind) VALUES (?,?,?)",
                    rusqlite::params![f, t, kind],
                )?;
                db::insert_event(
                    tx,
                    Some(f),
                    "relation_add",
                    None,
                    Some(&serde_json::json!({"kind": kind, "to": to})),
                )?;
                Ok(())
            })?;
        }
        RelOp::Rm { from, kind, to } => {
            let f = id::resolve(&conn, &from)?;
            let t = id::resolve(&conn, &to)?;
            db::with_tx(&mut conn, |tx| {
                let n = tx.execute(
                    "DELETE FROM relation WHERE from_task_id = ? AND to_task_id = ? AND kind = ?",
                    rusqlite::params![f, t, kind],
                )?;
                if n > 0 {
                    db::insert_event(
                        tx,
                        Some(f),
                        "relation_removed",
                        None,
                        Some(&serde_json::json!({"kind": kind, "to": to})),
                    )?;
                }
                Ok(())
            })?;
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
