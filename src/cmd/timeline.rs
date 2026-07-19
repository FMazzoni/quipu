//! Show the event log, for one task or across the store.
//!
#![doc = include_str!("../../docs/modules/timeline.md")]

use crate::store::{self, EventFilter};
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde_json::Value;

#[derive(Args, Debug)]
pub struct TimelineArgs {
    pub task: Option<String>,
    #[arg(long)]
    pub json: bool,
    #[arg(long = "kind")]
    pub kinds: Vec<String>,
    #[arg(long, default_value_t = 0)]
    pub since: i64,
}

pub fn run(db_path: &std::path::Path, a: TimelineArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let task_id = a
        .task
        .as_deref()
        .map(|s| id::resolve(&conn, s))
        .transpose()?;
    let filter = EventFilter {
        since_id: Some(a.since),
        task_id,
        kinds: &a.kinds,
        auto_only: false,
    };
    let rows = store::events(&conn, &filter)?;
    let collected: Vec<Value> = rows
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
            let kind = e["kind"].as_str().unwrap_or("");
            let body = crate::cmd::render::summarize_payload(kind, &e["payload"]);
            println!(
                "{}\t{}\t{}\t{}",
                e["ts"].as_str().unwrap_or(""),
                kind,
                e["agent_id"].as_str().unwrap_or("-"),
                body
            );
        }
    }
    Ok(())
}
