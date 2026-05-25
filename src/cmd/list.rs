use anyhow::Result;
use clap::Args;
use std::collections::HashMap;
use crate::db;

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Glob pattern (e.g. claude-code:*)
    #[arg(long = "assigned-to")] pub assigned_to: Option<String>,
    #[arg(long = "state")]       pub state: Option<String>,
    #[arg(long)]                 pub tag: Vec<String>,
    #[arg(long)]                 pub json: bool,
    /// Print each task's description on indented continuation lines.
    #[arg(long = "with-description")] pub with_description: bool,
}

pub fn run(db_path: &std::path::Path, a: ListArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    // Base task query with filters.
    let mut sql = String::from(
        "SELECT t.id, t.display_id, t.title, t.state, t.tier, t.description,
                (SELECT a.agent_id FROM assignment a WHERE a.task_id = t.id ORDER BY a.id DESC LIMIT 1) AS agent
           FROM task t WHERE 1=1");
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = &a.state {
        sql.push_str(" AND t.state = ?"); params.push(Box::new(s.clone()));
    }
    if let Some(who) = &a.assigned_to {
        sql.push_str(" AND (SELECT a.agent_id FROM assignment a WHERE a.task_id = t.id ORDER BY a.id DESC LIMIT 1) GLOB ?");
        params.push(Box::new(who.clone()));
    }
    for tag in &a.tag {
        sql.push_str(" AND EXISTS (SELECT 1 FROM tag WHERE tag.task_id = t.id AND tag.name = ?)");
        params.push(Box::new(tag.clone()));
    }
    sql.push_str(" ORDER BY t.id ASC");
    let mut stmt = conn.prepare(&sql)?;
    let pref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let core: Vec<(i64, String, String, String, Option<String>, Option<String>, Option<String>)> =
        stmt.query_map(pref.as_slice(), |r| Ok((
            r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?,
        )))?.collect::<Result<_, _>>()?;

    // Bulk-fetch tags, blocked_by, last_event for the selected ids.
    let ids: Vec<i64> = core.iter().map(|r| r.0).collect();
    let mut tags_by: HashMap<i64, Vec<String>> = HashMap::new();
    let mut blockers_by: HashMap<i64, Vec<String>> = HashMap::new();
    let mut last_event_by: HashMap<i64, serde_json::Value> = HashMap::new();
    if !ids.is_empty() {
        let placeholders = std::iter::repeat("?").take(ids.len()).collect::<Vec<_>>().join(",");
        let q = format!("SELECT task_id, name FROM tag WHERE task_id IN ({placeholders})");
        let mut s = conn.prepare(&q)?;
        let pref: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
        for r in s.query_map(pref.as_slice(), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (t, n) = r?; tags_by.entry(t).or_default().push(n);
        }
        let q = format!(
            "SELECT d.task_id, t2.display_id
               FROM dep d JOIN task t2 ON t2.id = d.depends_on_task_id
              WHERE d.task_id IN ({placeholders}) AND t2.state NOT IN ('done','cancelled')");
        let mut s = conn.prepare(&q)?;
        for r in s.query_map(pref.as_slice(), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (t, d) = r?; blockers_by.entry(t).or_default().push(d);
        }
        let q = format!(
            "SELECT task_id, kind, ts, payload FROM event
              WHERE id IN (SELECT MAX(id) FROM event WHERE task_id IN ({placeholders}) GROUP BY task_id)");
        let mut s = conn.prepare(&q)?;
        for r in s.query_map(pref.as_slice(), |r| {
            let payload: Option<String> = r.get(3)?;
            let payload_v: serde_json::Value = payload.as_deref()
                .map(serde_json::from_str).transpose().ok().flatten().unwrap_or(serde_json::Value::Null);
            Ok((r.get::<_, i64>(0)?, serde_json::json!({
                "kind": r.get::<_, String>(1)?, "ts": r.get::<_, String>(2)?, "payload": payload_v
            })))
        })? { let (t, v) = r?; last_event_by.insert(t, v); }
    }

    let mut out = Vec::with_capacity(core.len());
    for (id, did, title, state, tier, description, agent) in core {
        let mut obj = serde_json::json!({
            "id": id, "display_id": did, "title": title, "state": state, "tier": tier,
            "agent": agent,
            "tags": tags_by.remove(&id).unwrap_or_default(),
            "blocked_by": blockers_by.remove(&id).unwrap_or_default(),
            "last_event": last_event_by.remove(&id),
        });
        if let Some(d) = description {
            obj.as_object_mut().unwrap().insert("description".into(), serde_json::Value::String(d));
        }
        out.push(obj);
    }
    if a.json { println!("{}", serde_json::to_string(&out)?); }
    else {
        println!("ID\tSTATE\tAGENT\tTAGS\tTITLE");
        for r in &out {
            println!("{}\t{}\t{}\t{}\t{}",
                r["display_id"].as_str().unwrap_or(""),
                r["state"].as_str().unwrap_or(""),
                r["agent"].as_str().unwrap_or("-"),
                r["tags"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(",")).unwrap_or_default(),
                r["title"].as_str().unwrap_or(""));
            if a.with_description {
                if let Some(d) = r.get("description").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
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
