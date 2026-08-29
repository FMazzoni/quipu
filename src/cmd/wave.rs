//! Group in-flight work by state; `pending` lists only tasks with an unresolved dep.
//!
#![doc = include_str!("../../docs/modules/wave.md")]

use crate::outcome::Outcome;
use crate::{db, lease, store};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaveArgs {
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
}

impl crate::route::Served for WaveArgs {
    const VERB: &'static str = "wave";
    type Out = WaveView;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

/// The four state groups shown by `qp wave`, in display order.
const STATES: &[&str] = &["ready", "assigned", "running", "pending"];

/// The four groups, keyed by state, each already carrying its hold.
///
/// One object rather than four endpoints: the whole view is four filtered task
/// queries plus their last events and holds, and splitting it would make the
/// most-polled orchestrator view the most expensive one.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct WaveView(Value);

impl Outcome for WaveView {
    fn human(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        for &label in STATES {
            let arr = self.0.get(label).and_then(Value::as_array);
            let Some(arr) = arr.filter(|a| !a.is_empty()) else {
                continue;
            };
            lines.push(format!("## {label}"));
            for r in arr {
                // The stale marker trails the title rather than taking a column:
                // the four columns here are load-bearing elsewhere, and a hold
                // that has gone quiet is worth a sentence, not a flag. The text
                // itself is `lease::stale_marker`, shared with `list`, so the
                // two board views cannot describe one stranded ticket
                // differently.
                let stale = lease::stale_marker(&r["hold"]);
                lines.push(format!(
                    "  {:>7}  {:<14}  {:<10}  {}{}",
                    r["display_id"].as_str().unwrap_or(""),
                    r["agent"].as_str().unwrap_or("-"),
                    r["last_kind"].as_str().unwrap_or("-"),
                    r["title"].as_str().unwrap_or(""),
                    stale
                ));
            }
        }
        if lines.is_empty() {
            return "nothing in flight — run `qp status` for full state, or `qp list --state done` to see what shipped".to_string();
        }
        lines.join("\n")
    }
}

pub fn exec(db_path: &std::path::Path, _a: WaveArgs) -> Result<WaveView> {
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
        let arr: Vec<Value> = rows
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
        out.insert(label.to_string(), Value::Array(arr));
    }
    Ok(WaveView(Value::Object(out)))
}
