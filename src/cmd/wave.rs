use anyhow::Result;
use clap::Args;
use crate::db;

#[derive(Args, Debug)]
pub struct WaveArgs { #[arg(long)] pub json: bool }

pub fn run(db_path: &std::path::Path, a: WaveArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let groups = [("ready", db::STATE_READY),
                  ("assigned", db::STATE_ASSIGNED),
                  ("running", db::STATE_RUNNING),
                  ("blocked", db::STATE_BLOCKED)];
    let mut out = serde_json::Map::new();
    for (label, state) in groups {
        let mut s = conn.prepare(
            "SELECT t.display_id, t.title, t.state,
                    (SELECT a.agent_id FROM assignment a WHERE a.task_id = t.id ORDER BY a.id DESC LIMIT 1),
                    (SELECT kind FROM event e WHERE e.task_id = t.id ORDER BY e.id DESC LIMIT 1),
                    (SELECT ts   FROM event e WHERE e.task_id = t.id ORDER BY e.id DESC LIMIT 1)
               FROM task t WHERE t.state = ? ORDER BY t.id ASC")?;
        let rows = s.query_map([state], |r| Ok(serde_json::json!({
            "display_id": r.get::<_, String>(0)?,
            "title":      r.get::<_, String>(1)?,
            "state":      r.get::<_, String>(2)?,
            "agent":      r.get::<_, Option<String>>(3)?,
            "last_kind":  r.get::<_, Option<String>>(4)?,
            "last_ts":    r.get::<_, Option<String>>(5)?,
        })))?;
        let arr: Vec<_> = rows.collect::<Result<_, _>>()?;
        out.insert(label.to_string(), serde_json::Value::Array(arr));
    }
    let v = serde_json::Value::Object(out);
    if a.json { println!("{}", serde_json::to_string(&v)?); }
    else {
        for label in ["ready","assigned","running","blocked"] {
            let arr = v[label].as_array().unwrap();
            if arr.is_empty() { continue; }
            println!("## {label}");
            for r in arr {
                println!("  {:>5}  {:<14}  {:<10}  {}",
                    r["display_id"].as_str().unwrap_or(""),
                    r["agent"].as_str().unwrap_or("-"),
                    r["last_kind"].as_str().unwrap_or("-"),
                    r["title"].as_str().unwrap_or(""));
            }
        }
    }
    Ok(())
}
