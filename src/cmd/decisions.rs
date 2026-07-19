//! Show decision events.
//!
#![doc = include_str!("../../docs/modules/decisions.md")]

use crate::db;
use crate::store::{self, EventFilter};
use anyhow::Result;
use clap::Args;
use serde_json::Value;

#[derive(Args, Debug)]
pub struct DecisionsArgs {
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub auto_only: bool,
    /// Exclusive lower bound on event id: `--since 730` starts at 731.
    /// Same semantics as `timeline --since`, because it is the same clause.
    #[arg(long)]
    pub since: Option<i64>,
}

pub fn run(db_path: &std::path::Path, a: DecisionsArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let kinds = ["decision".to_string()];
    let filter = EventFilter {
        since_id: a.since,
        task_id: None,
        kinds: &kinds,
        auto_only: a.auto_only,
    };
    let collected: Vec<Value> = store::events(&conn, &filter)?
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "task": e.task,
                "ts": e.ts,
                "kind": e.kind,
                "agent_id": e.agent,
                "payload": e.payload,
            })
        })
        .collect();
    if a.json {
        println!("{}", serde_json::to_string(&collected)?);
    } else {
        for e in &collected {
            let auto = e["payload"]["auto"].as_bool().unwrap_or(false);
            let text = e["payload"]["text"].as_str().unwrap_or("");
            let flag = if auto { "[auto]" } else { "      " };
            println!(
                "{}\t{}\t{}\t{}\t{}",
                e["ts"].as_str().unwrap_or(""),
                e["task"].as_str().unwrap_or("-"),
                e["agent_id"].as_str().unwrap_or("-"),
                flag,
                text
            );
        }
    }
    Ok(())
}
