use anyhow::Result;
use clap::Args;
use std::collections::HashMap;
use crate::db;

#[derive(Args, Debug)]
pub struct TreeArgs {
    #[arg(long)] pub json: bool,
    #[arg(long)] pub tier: Option<String>,
    #[arg(long)] pub show_tags: bool,
}

pub fn run(db_path: &std::path::Path, a: TreeArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let mut tasks: Vec<(i64, String, String, String, Option<String>)> = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT id, display_id, title, state, tier FROM task
         WHERE (?1 IS NULL OR tier = ?1) ORDER BY id ASC")?;
    let rows = stmt.query_map(rusqlite::params![a.tier.as_deref()], |r|
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
    for r in rows { tasks.push(r?); }

    let mut deps: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut s = conn.prepare("SELECT task_id, depends_on_task_id FROM dep")?;
    for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (t,d) = r?; deps.entry(t).or_default().push(d);
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
        for (id, did, title, state, tier) in &tasks {
            out.push(serde_json::json!({
                "id": id, "display_id": did, "title": title, "state": state, "tier": tier,
                "depends_on": deps.get(id).cloned().unwrap_or_default(),
                "tags": tags_by.get(id).cloned().unwrap_or_default(),
            }));
        }
        println!("{}", serde_json::to_string(&out)?);
    } else {
        for (id, did, title, state, tier) in &tasks {
            let dep_s = deps.get(id).map(|v| v.iter().map(|d| format!("T{d}")).collect::<Vec<_>>().join(",")).unwrap_or_default();
            let tier_s = tier.as_deref().unwrap_or("-");
            let dep_part = if dep_s.is_empty() { "".into() } else { format!(" <- [{dep_s}]") };
            let tag_part = if a.show_tags {
                let ts = tags_by.get(id).map(|v| v.join(",")).unwrap_or_default();
                if ts.is_empty() { "".into() } else { format!(" #{ts}") }
            } else { "".into() };
            println!("{did:>5}  {state:<9}  {tier_s:<8}  {title}{dep_part}{tag_part}");
        }
    }
    Ok(())
}
