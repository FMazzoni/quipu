//! Polling event tail. Relies on the watch-correctness invariant:
//! events are only inserted inside `db::with_tx` (IMMEDIATE), so `event.id`
//! is gap-free as seen by readers — `WHERE id > last_seen` is safe.

use crate::db;
use crate::store::{self, EventFilter};
use anyhow::Result;
use clap::Args;
use std::time::Duration;

#[derive(Args, Debug)]
pub struct WatchArgs {
    /// Start tailing from this event id (exclusive).
    #[arg(long, default_value_t = 0)]
    pub since: i64,
    /// Poll interval in milliseconds.
    #[arg(long, default_value_t = 500)]
    pub interval_ms: u64,
    /// Emit JSON lines (default true since watch is for agents).
    #[arg(long, default_value_t = true)]
    pub json: bool,
    /// Stop after N empty-or-non-empty polling ticks. 0 = forever.
    #[arg(long, default_value_t = 0)]
    pub max_ticks: u64,
    /// Filter to events for this task only.
    #[arg(long)]
    pub task: Option<String>,
    /// Filter to these event kinds (repeatable).
    #[arg(long = "kind")]
    pub kinds: Vec<String>,
}

pub fn run(db_path: &std::path::Path, a: WatchArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let task_id = a
        .task
        .as_deref()
        .map(|s| crate::id::resolve(&conn, s))
        .transpose()?;

    let mut last_seen = a.since;
    let mut ticks: u64 = 0;
    loop {
        let filter = EventFilter {
            since_id: Some(last_seen),
            task_id,
            kinds: &a.kinds,
            auto_only: false,
        };
        let rows = store::events(&conn, &filter)?;
        for e in rows {
            let v = serde_json::json!({
                "id": e.id, "task": e.task, "ts": e.ts, "kind": e.kind,
                "agent_id": e.agent, "payload": e.payload,
            });
            if a.json {
                println!("{}", serde_json::to_string(&v)?);
            } else {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    e.ts,
                    e.task.unwrap_or_else(|| "-".into()),
                    e.kind,
                    e.agent.unwrap_or_else(|| "-".into()),
                    e.payload
                );
            }
            last_seen = e.id;
        }
        ticks += 1;
        if a.max_ticks > 0 && ticks >= a.max_ticks {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(a.interval_ms));
    }
}
