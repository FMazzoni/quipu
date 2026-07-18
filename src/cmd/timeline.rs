use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde_json::Value;

#[derive(Args, Debug)]
pub struct TimelineArgs {
    pub task: Option<String>,
    #[arg(long)]
    pub json: bool,
    #[arg(long = "kind")]
    pub kinds: Vec<String>,
    #[arg(long, default_value_t = 0)]
    pub since: i64,
}

pub fn run(db_path: &std::path::Path, a: TimelineArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let task_id = a
        .task
        .as_deref()
        .map(|s| id::resolve(&conn, s))
        .transpose()?;
    let mut sql = String::from(
        "SELECT e.id, t.display_id, e.ts, e.kind, e.agent_id, e.payload
           FROM event e LEFT JOIN task t ON t.id = e.task_id
          WHERE e.id > ?1",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(a.since)];
    if let Some(tid) = task_id {
        sql.push_str(" AND e.task_id = ?");
        sql.push_str(&format!("{}", params.len() + 1));
        params.push(Box::new(tid));
    }
    if !a.kinds.is_empty() {
        sql.push_str(" AND e.kind IN (");
        for (i, _) in a.kinds.iter().enumerate() {
            if i > 0 {
                sql.push(',');
            }
            sql.push_str(&format!("?{}", params.len() + 1 + i));
        }
        sql.push(')');
        for k in &a.kinds {
            params.push(Box::new(k.clone()));
        }
    }
    sql.push_str(" ORDER BY e.id ASC");
    let mut stmt = conn.prepare(&sql)?;
    let pref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(pref.as_slice(), |r| {
        let payload: Option<String> = r.get(5)?;
        let payload_v: Value = payload
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
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
    if a.json {
        println!("{}", serde_json::to_string(&collected)?);
    } else {
        for e in &collected {
            let kind = e["kind"].as_str().unwrap_or("");
            let body = summarize_payload(kind, &e["payload"]);
            println!(
                "{}\t{}\t{}\t{}",
                e["ts"].as_str().unwrap_or(""),
                kind,
                e["agent_id"].as_str().unwrap_or("-"),
                body
            );
        }
    }
    Ok(())
}

fn summarize_payload(kind: &str, p: &Value) -> String {
    match kind {
        "state_change" => p["to"].as_str().unwrap_or("").to_string(),
        "decision" => {
            let text = p["text"].as_str().unwrap_or("");
            if p["auto"].as_bool().unwrap_or(false) {
                format!("[auto] {text}")
            } else {
                text.to_string()
            }
        }
        "dep_added" | "dep_removed" => p["on"].as_str().unwrap_or("").to_string(),
        "edit" => {
            if let Some(obj) = p["changes"].as_object() {
                obj.keys().cloned().collect::<Vec<_>>().join(",")
            } else {
                String::new()
            }
        }
        "blocker" => p["title"].as_str().unwrap_or("").to_string(),
        "tag_added" | "tag_removed" => p["name"].as_str().unwrap_or("").to_string(),
        _ => {
            let s = serde_json::to_string(p).unwrap_or_default();
            if s.len() > 80 {
                format!("{}...", &s[..80])
            } else {
                s
            }
        }
    }
}
