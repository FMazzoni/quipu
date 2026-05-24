use anyhow::Result;
use clap::Args;
use serde_json::Value;
use crate::cmd::timeline::{run as run_timeline, TimelineArgs};
use crate::db;

#[derive(Args, Debug)]
pub struct DecisionsArgs {
    #[arg(long)] pub json: bool,
    #[arg(long)] pub auto_only: bool,
}

pub fn run(db_path: &std::path::Path, a: DecisionsArgs) -> Result<()> {
    if !a.auto_only {
        return run_timeline(db_path, TimelineArgs {
            task: None, json: a.json, kinds: vec!["decision".into()], since: 0,
        });
    }
    // --auto-only: filter decisions whose payload has auto=1 at the SQL layer.
    let conn = db::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT e.id, t.display_id, e.ts, e.kind, e.agent_id, e.payload
           FROM event e LEFT JOIN task t ON t.id = e.task_id
          WHERE e.kind = 'decision'
            AND e.payload IS NOT NULL
            AND json_extract(e.payload, '$.auto') = 1
          ORDER BY e.id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        let payload: Option<String> = r.get(5)?;
        let payload_v: Value = payload.as_deref()
            .map(serde_json::from_str).transpose().ok().flatten().unwrap_or(Value::Null);
        Ok(serde_json::json!({
            "id": r.get::<_, i64>(0)?,
            "task": r.get::<_, Option<String>>(1)?,
            "ts": r.get::<_, String>(2)?,
            "kind": r.get::<_, String>(3)?,
            "agent_id": r.get::<_, Option<String>>(4)?,
            "payload": payload_v,
        }))
    })?;
    let collected: Vec<Value> = rows.collect::<Result<_, _>>()?;
    if a.json { println!("{}", serde_json::to_string(&collected)?); }
    else {
        for e in &collected {
            println!("{}\t{}\t{}\t{}\t{}",
                e["ts"].as_str().unwrap_or(""),
                e["task"].as_str().unwrap_or("-"),
                e["kind"].as_str().unwrap_or(""),
                e["agent_id"].as_str().unwrap_or("-"),
                e["payload"]);
        }
    }
    Ok(())
}
