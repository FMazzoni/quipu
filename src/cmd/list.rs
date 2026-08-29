//! List tasks with filters.
//!
#![doc = include_str!("../../docs/modules/list.md")]

use crate::outcome::Outcome;
use crate::{db, store};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    /// Glob pattern (e.g. claude-code:*)
    #[arg(long = "assigned-to")]
    #[serde(default)]
    pub assigned_to: Option<String>,
    #[arg(long = "state")]
    #[serde(default)]
    pub state: Option<db::State>,
    /// Glob pattern (e.g. commit:5c5b30*); repeatable, all must match
    #[arg(long)]
    #[serde(default)]
    pub tag: Vec<String>,
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
    /// Print each task's description on indented continuation lines.
    #[arg(long = "with-description")]
    #[serde(skip)]
    pub with_description: bool,
}

impl crate::route::Served for ListArgs {
    const VERB: &'static str = "list";
    type Out = ListView;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
    fn restore_view(&self, out: &mut Self::Out) {
        out.with_description = self.with_description;
    }
}

/// The rows `qp list` renders, exactly as `--json` has always emitted them.
///
/// `#[serde(transparent)]` is load-bearing: the wire body is the bare array the
/// CLI already printed, so a consumer that piped `qp list --json` into `jq`
/// reads the same bytes from the socket. `with_description` is skipped because
/// it changes how many lines `human` prints and nothing about the data.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct ListView {
    rows: Value,
    #[serde(skip)]
    with_description: bool,
}

impl Outcome for ListView {
    fn human(&self) -> String {
        let mut out = String::from("ID\tSTATE\tAGENT\tTAGS\tTITLE");
        for r in self.rows.as_array().into_iter().flatten() {
            out.push('\n');
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}",
                r["display_id"].as_str().unwrap_or(""),
                r["state"].as_str().unwrap_or(""),
                r["agent"].as_str().unwrap_or("-"),
                r["tags"]
                    .as_array()
                    .map(|a| a
                        .iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default(),
                r["title"].as_str().unwrap_or("")
            ));
            if self.with_description {
                if let Some(d) = r
                    .get("description")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    for line in crate::cmd::show::wrap_text(d, 80).iter().take(3) {
                        out.push_str(&format!("\n    {line}"));
                    }
                }
            }
        }
        out
    }
}

pub fn exec(db_path: &std::path::Path, a: ListArgs) -> Result<ListView> {
    let conn = db::open(db_path)?;
    let filter = store::TaskFilter {
        state: a.state.map(|s| s.as_str()),
        assigned_to_glob: a.assigned_to.as_deref(),
        tag_globs: &a.tag,
        tier: None,
    };
    let core = store::tasks(&conn, &filter)?;

    let ids: Vec<i64> = core.iter().map(|r| r.id).collect();
    let mut tags_by = store::tags_by_task(&conn, &ids)?;
    let mut blockers_by = store::unresolved_blockers_by_task(&conn, &ids)?;
    let mut last_event_by = store::last_event_by_task(&conn, &ids)?;

    let mut out = Vec::with_capacity(core.len());
    for row in core {
        let obj = serde_json::json!({
            "id": row.id, "display_id": row.display_id, "title": row.title,
            "state": row.state, "tier": row.tier,
            "description": row.description,
            "agent": row.agent,
            "tags": tags_by.remove(&row.id).unwrap_or_default(),
            "blocked_by": blockers_by.remove(&row.id).unwrap_or_default(),
            "last_event": last_event_by.remove(&row.id),
        });
        out.push(obj);
    }
    Ok(ListView {
        rows: Value::Array(out),
        with_description: a.with_description,
    })
}
