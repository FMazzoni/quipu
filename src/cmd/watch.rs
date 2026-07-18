//! Polling event tail. Relies on the watch-correctness invariant:
//! events are only inserted inside `db::with_tx` (IMMEDIATE), so `event.id`
//! is gap-free as seen by readers — `WHERE id > last_seen` is safe.

use crate::db;
use anyhow::Result;
use clap::Args;
use std::time::Duration;

#[derive(Args, Debug)]
pub struct WatchArgs {
    /// Start tailing from this event id (exclusive).
    #[arg(long, default_value_t = 0)]
    pub since: i64,
    /// Poll interval in milliseconds.
    #[arg(long, default_value_t = 500)]
    pub interval_ms: u64,
    /// Emit JSON lines (default true since watch is for agents).
    #[arg(long, default_value_t = true)]
    pub json: bool,
    /// Stop after N empty-or-non-empty polling ticks. 0 = forever.
    #[arg(long, default_value_t = 0)]
    pub max_ticks: u64,
    /// Filter to events for this task only.
    #[arg(long)]
    pub task: Option<String>,
    /// Filter to these event kinds (repeatable).
    #[arg(long = "kind")]
    pub kinds: Vec<String>,
}

pub fn run(db_path: &std::path::Path, a: WatchArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let task_id = a
        .task
        .as_deref()
        .map(|s| crate::id::resolve(&conn, s))
        .transpose()?;

    let mut last_seen = a.since;
    let mut ticks: u64 = 0;
    loop {
        let mut sql = String::from(
            "SELECT e.id, t.display_id, e.ts, e.kind, e.agent_id, e.payload
               FROM event e LEFT JOIN task t ON t.id = e.task_id
              WHERE e.id > ?1",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(last_seen)];
        if let Some(tid) = task_id {
            sql.push_str(&format!(" AND e.task_id = ?{}", params.len() + 1));
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
        let rows: Vec<(
            i64,
            Option<String>,
            String,
            String,
            Option<String>,
            Option<String>,
        )> = stmt
            .query_map(pref.as_slice(), |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        for (id, did, ts, kind, agent, payload) in rows {
            let payload_v: serde_json::Value = payload
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .ok()
                .flatten()
                .unwrap_or(serde_json::Value::Null);
            let v = serde_json::json!({
                "id": id, "task": did, "ts": ts, "kind": kind,
                "agent_id": agent, "payload": payload_v,
            });
            if a.json {
                println!("{}", serde_json::to_string(&v)?);
            } else {
                println!(
                    "{ts}\t{}\t{kind}\t{}\t{payload_v}",
                    did.unwrap_or_else(|| "-".into()),
                    agent.unwrap_or_else(|| "-".into())
                );
            }
            last_seen = id;
        }
        ticks += 1;
        if a.max_ticks > 0 && ticks >= a.max_ticks {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(a.interval_ms));
    }
}
