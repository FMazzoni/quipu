use anyhow::Result;
use clap::Args;
use std::collections::{HashMap, HashSet};
use crate::{db, id};

#[derive(Args, Debug)]
pub struct TreeArgs {
    /// Optional task id — when present, restrict output to this task + its transitive deps.
    pub task: Option<String>,
    #[arg(long)] pub json: bool,
    #[arg(long)] pub tier: Option<String>,
    #[arg(long)] pub show_tags: bool,
    /// Print each task's description on indented continuation lines.
    #[arg(long = "with-description")] pub with_description: bool,
}

pub fn run(db_path: &std::path::Path, a: TreeArgs) -> Result<()> {
    let conn = db::open(db_path)?;

    // If a root task is given, compute its transitive dep subtree (inclusive).
    let subtree: Option<HashSet<i64>> = if let Some(t) = &a.task {
        let root = id::resolve(&conn, t)?;
        let mut s = conn.prepare(
            "WITH RECURSIVE sub(id) AS (
                SELECT ?1
                UNION
                SELECT d.depends_on_task_id FROM dep d JOIN sub ON d.task_id = sub.id
             ) SELECT id FROM sub")?;
        let ids: HashSet<i64> = s.query_map([root], |r| r.get::<_, i64>(0))?
            .collect::<Result<_, _>>()?;
        Some(ids)
    } else { None };

    let mut tasks: Vec<(i64, String, String, String, Option<String>, Option<String>)> = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT id, display_id, title, state, tier, description FROM task
         WHERE (?1 IS NULL OR tier = ?1) ORDER BY id ASC")?;
    let rows = stmt.query_map(rusqlite::params![a.tier.as_deref()], |r|
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
    for r in rows {
        let row = r?;
        if let Some(set) = &subtree {
            if !set.contains(&row.0) { continue; }
        }
        tasks.push(row);
    }

    let mut deps: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut s = conn.prepare("SELECT task_id, depends_on_task_id FROM dep")?;
    for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (t,d) = r?; deps.entry(t).or_default().push(d);
    }

    // id -> display_id lookup for rendering dep refs in display-id format.
    let mut display_by_id: HashMap<i64, String> = HashMap::new();
    let mut s = conn.prepare("SELECT id, display_id FROM task")?;
    for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (i, d) = r?; display_by_id.insert(i, d);
    }

    let mut tags_by: HashMap<i64, Vec<String>> = HashMap::new();
    if a.show_tags || a.json {
        let mut s = conn.prepare("SELECT task_id, name FROM tag")?;
        for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (t, n) = r?; tags_by.entry(t).or_default().push(n);
        }
    }

    if a.json {
        let mut out = Vec::new();
        for (id, did, title, state, tier, description) in &tasks {
            let mut obj = serde_json::json!({
                "id": id, "display_id": did, "title": title, "state": state, "tier": tier,
                "depends_on": deps.get(id).cloned().unwrap_or_default(),
                "tags": tags_by.get(id).cloned().unwrap_or_default(),
            });
            if let Some(d) = description {
                obj.as_object_mut().unwrap().insert("description".into(), serde_json::Value::String(d.clone()));
            }
            out.push(obj);
        }
        println!("{}", serde_json::to_string(&out)?);
    } else {
        for (id, did, title, state, tier, description) in &tasks {
            let dep_s = deps.get(id).map(|v| v.iter().map(|d|
                display_by_id.get(d).cloned().unwrap_or_else(|| format!("T{d}"))
            ).collect::<Vec<_>>().join(",")).unwrap_or_default();
            let tier_s = tier.as_deref().unwrap_or("-");
            let dep_part = if dep_s.is_empty() { "".into() } else { format!(" <- [{dep_s}]") };
            let tag_part = if a.show_tags {
                let ts = tags_by.get(id).map(|v| v.join(",")).unwrap_or_default();
                if ts.is_empty() { "".into() } else { format!(" #{ts}") }
            } else { "".into() };
            println!("{did:>5}  {state:<9}  {tier_s:<8}  {title}{dep_part}{tag_part}");
            if a.with_description {
                if let Some(d) = description.as_deref().filter(|s| !s.is_empty()) {
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
