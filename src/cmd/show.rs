//! `qp show <task>` — single-ticket detail view.
//!
//! Human mode: header line (id/state/tier/tags-summary), title, metadata,
//! description, and the last 10 timeline events for the task.
//!
//! JSON mode: full task record (mirroring `qp list --json` for that one task)
//! plus a `recent_events` field with the last 10 events.

use crate::{db, id, store};
use anyhow::Result;
use clap::Args;
use serde_json::Value;

#[derive(Args, Debug)]
pub struct ShowArgs {
    /// Task display id (e.g. QP-26)
    pub task: String,
    /// Emit JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

pub fn run(db_path: &std::path::Path, a: ShowArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let tid = resolved.id;
    let display_id = resolved.display_id;

    // Core task record.
    let (title, state, tier, description, created_at): (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = conn.query_row(
        "SELECT title, state, tier, description, created_at FROM task WHERE id = ?1",
        [tid],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;

    let agent = store::latest_agent(&conn, tid)?;

    let mut tag_stmt = conn.prepare("SELECT name FROM tag WHERE task_id = ?1 ORDER BY name")?;
    let mut tags: Vec<String> = tag_stmt
        .query_map([tid], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    tags.sort();

    let blocked_by = store::unresolved_blockers_by_task(&conn, &[tid])?
        .remove(&tid)
        .unwrap_or_default();

    // Recent events: last 10, newest first.
    let mut s = conn.prepare(
        "SELECT ts, kind, agent_id, payload FROM event
          WHERE task_id = ?1 ORDER BY id DESC LIMIT 10",
    )?;
    let mut events: Vec<(String, String, Option<String>, Value)> = s
        .query_map([tid], |r| {
            let payload: Option<String> = r.get(3)?;
            let payload_v: Value = payload
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .ok()
                .flatten()
                .unwrap_or(Value::Null);
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                payload_v,
            ))
        })?
        .collect::<Result<_, _>>()?;

    if a.json {
        // last_event is the newest (events[0] if present).
        let last_event = events.first().map(|(ts, kind, _agent, payload)| {
            serde_json::json!({
                "kind": kind, "ts": ts, "payload": payload.clone(),
            })
        });
        // recent_events ordered oldest-first for friendlier consumption.
        let recent_events: Vec<Value> = events
            .iter()
            .rev()
            .map(|(ts, kind, agent, payload)| {
                serde_json::json!({
                    "ts": ts, "kind": kind, "agent_id": agent, "payload": payload.clone(),
                })
            })
            .collect();
        let mut obj = serde_json::json!({
            "id": tid,
            "display_id": display_id,
            "title": title,
            "state": state,
            "tier": tier,
            "agent": agent,
            "tags": tags,
            "blocked_by": blocked_by,
            "last_event": last_event,
            "recent_events": recent_events,
        });
        if let Some(d) = description.as_ref() {
            obj.as_object_mut()
                .unwrap()
                .insert("description".into(), Value::String(d.clone()));
        }
        if let Some(c) = created_at.as_ref() {
            obj.as_object_mut()
                .unwrap()
                .insert("created_at".into(), Value::String(c.clone()));
        }
        println!("{}", serde_json::to_string(&obj)?);
        return Ok(());
    }

    // Human mode.
    let tier_str = tier.as_deref().unwrap_or("-");
    let tag_summary = if tags.is_empty() {
        String::from("-")
    } else {
        tags.join(", ")
    };
    println!("{}  {}  {}  {}", display_id, state, tier_str, tag_summary);
    println!("{}", title);
    println!();
    println!("  agent: {}", agent.as_deref().unwrap_or("—"));
    if let Some(c) = &created_at {
        println!("  created: {}", c);
    }
    if !tags.is_empty() {
        println!("  tags: {}", tags.join(", "));
    }
    if !blocked_by.is_empty() {
        println!("  blocked_by: {}", blocked_by.join(", "));
    }

    if let Some(d) = description.as_deref().filter(|s| !s.is_empty()) {
        println!();
        for line in wrap_text(d, 80) {
            println!("{}", line);
        }
    }

    println!();
    println!("Recent events ({}):", events.len());
    // Render oldest -> newest for readability.
    events.reverse();
    for (ts, kind, _agent, payload) in &events {
        // Short HH:MM:SS slice if RFC3339; otherwise full ts.
        let short_ts = ts.get(11..19).unwrap_or(ts.as_str());
        let summary = crate::cmd::render::summarize_payload(kind, payload);
        println!("  {}  {:<14}  {}", short_ts, kind, summary);
    }
    Ok(())
}

/// Hard-wrap text to `width` columns, splitting on whitespace. Preserves
/// existing newline boundaries.
pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if line.is_empty() {
                line.push_str(word);
            } else if line.len() + 1 + word.len() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                line.push_str(word);
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}
