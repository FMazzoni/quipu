//! Group in-flight work by state; `pending` lists only tasks with an unresolved dep.
//!
#![doc = include_str!("../../docs/modules/wave.md")]

use crate::{db, lease, store};
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
            tag_globs: &[],
            tier: None,
        };
        let mut rows = store::tasks(&conn, &filter)?;

        if label == "pending" {
            let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
            // Mode-agnostic on purpose: the question here is "is this genuinely
            // waiting on something", and a container waiting on its own contents
            // qualifies even though it has no *blockers*. Using the blockers-only
            // query drops every wave parent out of this group.
            let waiting = store::unresolved_deps_by_task(&conn, &ids)?;
            // A frozen slice has no unresolved dep of its own — the blocker is
            // on a container above it — so it satisfies neither half of this
            // filter on its own and would disappear from every view.
            let frozen = store::frozen_ids(&conn)?;
            rows.retain(|r| {
                waiting.get(&r.id).is_some_and(|v| !v.is_empty()) || frozen.contains(&r.id)
            });
        }

        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        let last_event = store::last_event_by_task(&conn, &ids)?;
        // A stranded ticket has to be legible in the view an orchestrator
        // already reads, or finding it needs a command nobody thinks to run.
        let holds = lease::holds_by_task(&conn, &ids)?;
        let arr: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                let ev = last_event.get(&r.id);
                let hold = holds.get(&r.id);
                serde_json::json!({
                    "display_id": r.display_id,
                    "title": r.title,
                    "state": r.state,
                    "agent": r.agent,
                    "last_kind": ev.and_then(|v| v.get("kind")).and_then(|v| v.as_str()),
                    "last_ts": ev.and_then(|v| v.get("ts")).and_then(|v| v.as_str()),
                    "stale": hold.is_some_and(|h| h.stale),
                    "hold": hold.map(lease::Hold::to_json),
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
                // The stale marker trails the title rather than taking a column:
                // the four columns here are load-bearing elsewhere, and a hold
                // that has gone quiet is worth a sentence, not a flag.
                let stale = if r["stale"].as_bool().unwrap_or(false) {
                    let host = r["hold"]["host"].as_str().unwrap_or("unknown host");
                    format!(
                        "   [STALE — quiet since {}, on {}]",
                        r["hold"]["last_contact"].as_str().unwrap_or("?"),
                        host
                    )
                } else {
                    String::new()
                };
                println!(
                    "  {:>7}  {:<14}  {:<10}  {}{}",
                    r["display_id"].as_str().unwrap_or(""),
                    r["agent"].as_str().unwrap_or("-"),
                    r["last_kind"].as_str().unwrap_or("-"),
                    r["title"].as_str().unwrap_or(""),
                    stale
                );
            }
        }
        if !any {
            println!("nothing in flight — run `qp status` for full state, or `qp list --state done` to see what shipped");
        }
    }
    Ok(())
}
