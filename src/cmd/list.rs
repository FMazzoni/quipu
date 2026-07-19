//! List tasks with filters.

use crate::{db, store};
use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Glob pattern (e.g. claude-code:*)
    #[arg(long = "assigned-to")]
    pub assigned_to: Option<String>,
    #[arg(long = "state")]
    pub state: Option<db::State>,
    #[arg(long)]
    pub tag: Vec<String>,
    #[arg(long)]
    pub json: bool,
    /// Print each task's description on indented continuation lines.
    #[arg(long = "with-description")]
    pub with_description: bool,
}

pub fn run(db_path: &std::path::Path, a: ListArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let filter = store::TaskFilter {
        state: a.state.map(|s| s.as_str()),
        assigned_to_glob: a.assigned_to.as_deref(),
        tags: &a.tag,
        tier: None,
    };
    let core = store::tasks(&conn, &filter)?;

    // Bulk-fetch tags, blocked_by, last_event for the selected ids.
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
    if a.json {
        println!("{}", serde_json::to_string(&out)?);
    } else {
        println!("ID\tSTATE\tAGENT\tTAGS\tTITLE");
        for r in &out {
            println!(
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
            );
            if a.with_description {
                if let Some(d) = r
                    .get("description")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    let lines = crate::cmd::show::wrap_text(d, 80);
                    for line in lines.iter().take(3) {
                        println!("    {}", line);
                    }
                }
            }
        }
    }
    Ok(())
}
