//! Group in-flight work by state: ready, assigned, running, pending.

use crate::{db, store};
use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct WaveArgs {
    #[arg(long)]
    pub json: bool,
}

/// The four state groups shown by `qp wave`, in display order.
const STATES: &[&str] = &["ready", "assigned", "running", "pending"];

pub fn run(db_path: &std::path::Path, a: WaveArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let mut out = serde_json::Map::new();
    for &label in STATES {
        let filter = store::TaskFilter {
            state: Some(label),
            assigned_to_glob: None,
            tags: &[],
            tier: None,
        };
        let mut rows = store::tasks(&conn, &filter)?;

        // Pending tasks appear here iff they have at least one unresolved dep
        // (depends_on task is not done/cancelled). This is broader than the
        // skill-layer `kind:blocker` convention — any unresolved dep qualifies.
        // Pending-without-unresolved-deps tasks stay hidden from the wave view.
        if label == "pending" {
            let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
            let blockers = store::unresolved_blockers_by_task(&conn, &ids)?;
            rows.retain(|r| blockers.get(&r.id).is_some_and(|v| !v.is_empty()));
        }

        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        let last_event = store::last_event_by_task(&conn, &ids)?;
        let arr: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                let ev = last_event.get(&r.id);
                serde_json::json!({
                    "display_id": r.display_id,
                    "title": r.title,
                    "state": r.state,
                    "agent": r.agent,
                    "last_kind": ev.and_then(|v| v.get("kind")).and_then(|v| v.as_str()),
                    "last_ts": ev.and_then(|v| v.get("ts")).and_then(|v| v.as_str()),
                })
            })
            .collect();
        out.insert(label.to_string(), serde_json::Value::Array(arr));
    }
    let v = serde_json::Value::Object(out);
    if a.json {
        println!("{}", serde_json::to_string(&v)?);
    } else {
        let mut any = false;
        for &label in STATES {
            let arr = v[label].as_array().unwrap();
            if arr.is_empty() {
                continue;
            }
            any = true;
            println!("## {label}");
            for r in arr {
                println!(
                    "  {:>7}  {:<14}  {:<10}  {}",
                    r["display_id"].as_str().unwrap_or(""),
                    r["agent"].as_str().unwrap_or("-"),
                    r["last_kind"].as_str().unwrap_or("-"),
                    r["title"].as_str().unwrap_or("")
                );
            }
        }
        if !any {
            println!("nothing in flight — run `qp status` for full state, or `qp list --state done` to see what shipped");
        }
    }
    Ok(())
}
