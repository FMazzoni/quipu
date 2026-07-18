use crate::db;
use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct WaveArgs {
    #[arg(long)]
    pub json: bool,
}

const GROUPS: &[(&str, &str)] = &[
    ("ready",    "SELECT t.display_id, t.title, t.state, \
                         (SELECT a.agent_id FROM assignment a WHERE a.task_id=t.id ORDER BY a.id DESC LIMIT 1), \
                         (SELECT kind FROM event e WHERE e.task_id=t.id ORDER BY e.id DESC LIMIT 1), \
                         (SELECT ts   FROM event e WHERE e.task_id=t.id ORDER BY e.id DESC LIMIT 1) \
                  FROM task t WHERE t.state='ready' ORDER BY t.id ASC"),
    ("assigned", "SELECT t.display_id, t.title, t.state, \
                         (SELECT a.agent_id FROM assignment a WHERE a.task_id=t.id ORDER BY a.id DESC LIMIT 1), \
                         (SELECT kind FROM event e WHERE e.task_id=t.id ORDER BY e.id DESC LIMIT 1), \
                         (SELECT ts   FROM event e WHERE e.task_id=t.id ORDER BY e.id DESC LIMIT 1) \
                  FROM task t WHERE t.state='assigned' ORDER BY t.id ASC"),
    ("running",  "SELECT t.display_id, t.title, t.state, \
                         (SELECT a.agent_id FROM assignment a WHERE a.task_id=t.id ORDER BY a.id DESC LIMIT 1), \
                         (SELECT kind FROM event e WHERE e.task_id=t.id ORDER BY e.id DESC LIMIT 1), \
                         (SELECT ts   FROM event e WHERE e.task_id=t.id ORDER BY e.id DESC LIMIT 1) \
                  FROM task t WHERE t.state='running' ORDER BY t.id ASC"),
    // Pending tasks appear here iff they have at least one unresolved dep
    // (depends_on task is not done/cancelled). This is broader than the
    // skill-layer `kind:blocker` convention — any unresolved dep qualifies.
    // Pending-without-unresolved-deps tasks stay hidden from the wave view.
    ("pending",  "SELECT t.display_id, t.title, t.state, \
                         (SELECT a.agent_id FROM assignment a WHERE a.task_id=t.id ORDER BY a.id DESC LIMIT 1), \
                         (SELECT kind FROM event e WHERE e.task_id=t.id ORDER BY e.id DESC LIMIT 1), \
                         (SELECT ts   FROM event e WHERE e.task_id=t.id ORDER BY e.id DESC LIMIT 1) \
                  FROM task t WHERE t.state='pending' \
                    AND EXISTS (SELECT 1 FROM dep d JOIN task t2 ON t2.id=d.depends_on_task_id \
                                 WHERE d.task_id=t.id AND t2.state NOT IN ('done','cancelled')) \
                  ORDER BY t.id ASC"),
];

pub fn run(db_path: &std::path::Path, a: WaveArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let mut out = serde_json::Map::new();
    for (label, sql) in GROUPS {
        let mut s = conn.prepare(sql)?;
        let rows = s.query_map([], |r| {
            Ok(serde_json::json!({
                "display_id": r.get::<_, String>(0)?,
                "title":      r.get::<_, String>(1)?,
                "state":      r.get::<_, String>(2)?,
                "agent":      r.get::<_, Option<String>>(3)?,
                "last_kind":  r.get::<_, Option<String>>(4)?,
                "last_ts":    r.get::<_, Option<String>>(5)?,
            }))
        })?;
        let arr: Vec<_> = rows.collect::<Result<_, _>>()?;
        out.insert((*label).to_string(), serde_json::Value::Array(arr));
    }
    let v = serde_json::Value::Object(out);
    if a.json {
        println!("{}", serde_json::to_string(&v)?);
    } else {
        let mut any = false;
        for (label, _) in GROUPS {
            let arr = v[*label].as_array().unwrap();
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
