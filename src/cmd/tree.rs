//! Render the dependency DAG.

use crate::{db, id, store};
use anyhow::Result;
use clap::Args;
use std::collections::{HashMap, HashSet};

#[derive(Args, Debug)]
pub struct TreeArgs {
    /// Optional task id — when present, restrict output to this task + its transitive deps.
    pub task: Option<String>,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub tier: Option<String>,
    #[arg(long)]
    pub show_tags: bool,
    /// Print each task's description on indented continuation lines.
    #[arg(long = "with-description")]
    pub with_description: bool,
}

pub fn run(db_path: &std::path::Path, a: TreeArgs) -> Result<()> {
    let conn = db::open(db_path)?;

    // If a root task is given, compute its transitive dep subtree (inclusive).
    let subtree: Option<HashSet<i64>> = if let Some(t) = &a.task {
        let root = id::resolve(&conn, t)?;
        Some(store::subtree_ids(&conn, root)?)
    } else {
        None
    };

    let filter = store::TaskFilter {
        state: None,
        assigned_to_glob: None,
        tags: &[],
        tier: a.tier.as_deref(),
    };
    let mut tasks: Vec<store::TaskRow> = Vec::new();
    for row in store::tasks(&conn, &filter)? {
        if let Some(set) = &subtree {
            if !set.contains(&row.id) {
                continue;
            }
        }
        tasks.push(row);
    }

    let mut deps: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut s = conn.prepare("SELECT task_id, depends_on_task_id FROM dep")?;
    for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (t, d) = r?;
        deps.entry(t).or_default().push(d);
    }

    // id -> display_id lookup for rendering dep refs in display-id format.
    let mut display_by_id: HashMap<i64, String> = HashMap::new();
    let mut s = conn.prepare("SELECT id, display_id FROM task")?;
    for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (i, d) = r?;
        display_by_id.insert(i, d);
    }

    let mut tags_by: HashMap<i64, Vec<String>> = HashMap::new();
    if a.show_tags || a.json {
        let mut s = conn.prepare("SELECT task_id, name FROM tag")?;
        for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (t, n) = r?;
            tags_by.entry(t).or_default().push(n);
        }
    }

    if a.json {
        let mut out = Vec::new();
        for row in &tasks {
            let mut obj = serde_json::json!({
                "id": row.id, "display_id": row.display_id, "title": row.title,
                "state": row.state, "tier": row.tier,
                "depends_on": deps.get(&row.id).cloned().unwrap_or_default(),
                "tags": tags_by.get(&row.id).cloned().unwrap_or_default(),
            });
            if let Some(d) = &row.description {
                obj.as_object_mut()
                    .unwrap()
                    .insert("description".into(), serde_json::Value::String(d.clone()));
            }
            out.push(obj);
        }
        println!("{}", serde_json::to_string(&out)?);
    } else {
        for row in &tasks {
            let dep_s = deps
                .get(&row.id)
                .map(|v| {
                    v.iter()
                        .map(|d| {
                            display_by_id
                                .get(d)
                                .cloned()
                                .unwrap_or_else(|| format!("T{d}"))
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            let tier_s = row.tier.as_deref().unwrap_or("-");
            let dep_part = if dep_s.is_empty() {
                "".into()
            } else {
                format!(" <- [{dep_s}]")
            };
            let tag_part = if a.show_tags {
                let ts = tags_by
                    .get(&row.id)
                    .map(|v| v.join(","))
                    .unwrap_or_default();
                if ts.is_empty() {
                    "".into()
                } else {
                    format!(" #{ts}")
                }
            } else {
                "".into()
            };
            let (did, state, title) = (&row.display_id, &row.state, &row.title);
            println!("{did:>5}  {state:<9}  {tier_s:<8}  {title}{dep_part}{tag_part}");
            if a.with_description {
                if let Some(d) = row.description.as_deref().filter(|s| !s.is_empty()) {
                    let lines = crate::cmd::show::wrap_text(d, 80);
                    for line in lines.iter().take(3) {
                        println!("       {}", line);
                    }
                }
            }
        }
    }
    Ok(())
}
