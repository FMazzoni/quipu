//! `qp report` — emit a structured JSON snapshot of the qp store.
//!
#![doc = include_str!("../../docs/modules/report.md")]

use crate::{db, id, store};
use anyhow::Result;
use clap::Args;
use serde_json::Value;
use std::collections::HashSet;
use std::io::Write;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[derive(Args, Debug)]
pub struct ReportArgs {
    /// Filter events to this window. Accepts `24h`, `7d`, or RFC3339 date (e.g. `2026-05-20`).
    #[arg(long)]
    pub since: Option<String>,
    /// Scope to a wave: restrict to the given task + its transitive dep subtree.
    #[arg(long)]
    pub wave: Option<String>,
    /// Emit JSON. This is the only supported output format now; the flag is
    /// accepted for backward compatibility (existing scripts pass it) but has
    /// no effect — report always emits JSON.
    #[arg(long)]
    pub json: bool,
    /// Write to this path instead of stdout.
    #[arg(long)]
    pub output: Option<std::path::PathBuf>,
    /// Single-ticket mode: emit the full detail (incl. parents/children/uncapped events) for one ticket.
    #[arg(long, conflicts_with = "all_tickets")]
    pub ticket: Option<String>,
    /// Bulk mode: emit a JSON array of per-ticket detail objects, scoped by --since/--wave.
    #[arg(long = "all-tickets", conflicts_with = "ticket")]
    pub all_tickets: bool,
}

pub fn run(db_path: &std::path::Path, a: ReportArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let since_iso = a.since.as_deref().map(parse_since).transpose()?;
    let subtree = a
        .wave
        .as_deref()
        .map(|t| resolve_subtree(&conn, t))
        .transpose()?;

    // Per-ticket mode: full detail for one ticket.
    if let Some(tref) = a.ticket.as_deref() {
        let resolved = id::resolve_full(&conn, tref)?;
        let detail = collect_ticket(&conn, resolved.id)?;
        let body = serde_json::to_string(&ticket_detail_json(&detail))?;
        write_output(a.output.as_deref(), &body)?;
        return Ok(());
    }

    // Bulk mode: array of per-ticket detail, scoped.
    if a.all_tickets {
        let scope_ids = ticket_ids_in_scope(&conn, since_iso.as_deref(), subtree.as_ref())?;
        let mut arr = Vec::with_capacity(scope_ids.len());
        for tid in scope_ids {
            let detail = collect_ticket(&conn, tid)?;
            arr.push(ticket_detail_json(&detail));
        }
        let body = serde_json::to_string(&Value::Array(arr))?;
        write_output(a.output.as_deref(), &body)?;
        return Ok(());
    }

    // Default: board payload.
    let payload = collect_json(&conn, since_iso.as_deref(), subtree.as_ref())?;
    let body = serde_json::to_string(&payload)?;
    write_output(a.output.as_deref(), &body)?;
    Ok(())
}

fn write_output(path: Option<&std::path::Path>, body: &str) -> Result<()> {
    if let Some(path) = path {
        let mut f = std::fs::File::create(path)?;
        f.write_all(body.as_bytes())?;
        f.write_all(b"\n")?;
    } else {
        println!("{body}");
    }
    Ok(())
}

// ---------- Per-ticket collection ----------

struct TicketDetail {
    display_id: String,
    title: String,
    state: String,
    tier: Option<String>,
    agent: Option<String>,
    description: Option<String>,
    created_at: Option<String>,
    tags: Vec<String>,
    parents: Vec<(String, String, String)>, // display_id, title, state — this depends on
    children: Vec<(String, String, String)>, // display_id, title, state — depend on this
    events: Vec<store::EventRow>,           // chronological asc, uncapped
}

fn ticket_detail_json(t: &TicketDetail) -> Value {
    let parents: Vec<Value> = t
        .parents
        .iter()
        .map(|(did, title, state)| {
            serde_json::json!({"display_id": did, "title": title, "state": state})
        })
        .collect();
    let children: Vec<Value> = t
        .children
        .iter()
        .map(|(did, title, state)| {
            serde_json::json!({"display_id": did, "title": title, "state": state})
        })
        .collect();
    let events: Vec<Value> = t
        .events
        .iter()
        .map(|e| {
            serde_json::json!({
                "ts": e.ts,
                "kind": e.kind,
                "agent_id": e.agent,
                "payload": e.payload,
            })
        })
        .collect();
    serde_json::json!({
        "display_id": t.display_id,
        "title": t.title,
        "state": t.state,
        "tier": t.tier,
        "agent": t.agent,
        "description": t.description,
        "created_at": t.created_at,
        "tags": t.tags,
        "parents": parents,
        "children": children,
        "events": events,
    })
}

fn collect_ticket(conn: &rusqlite::Connection, tid: i64) -> Result<TicketDetail> {
    let (display_id, title, state, tier, description, created_at): (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = conn.query_row(
        "SELECT display_id, title, state, tier, description, created_at FROM task WHERE id = ?1",
        [tid],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    let agent = store::latest_agent(conn, tid)?;
    let mut tags = store::tags_by_task(conn, &[tid])?
        .remove(&tid)
        .unwrap_or_default();
    tags.sort();

    let mut p_stmt = conn.prepare(
        "SELECT t.display_id, t.title, t.state FROM dep d JOIN task t ON t.id = d.depends_on_task_id
          WHERE d.task_id = ?1 ORDER BY t.id")?;
    let parents: Vec<(String, String, String)> = p_stmt
        .query_map([tid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut c_stmt = conn.prepare(
        "SELECT t.display_id, t.title, t.state FROM dep d JOIN task t ON t.id = d.task_id
          WHERE d.depends_on_task_id = ?1 ORDER BY t.id",
    )?;
    let children: Vec<(String, String, String)> = c_stmt
        .query_map([tid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<_, _>>()?;

    // Full timeline for this ticket, oldest-first, uncapped.
    let events = store::events(
        conn,
        &store::EventFilter {
            task_id: Some(tid),
            ..Default::default()
        },
    )?;

    Ok(TicketDetail {
        display_id,
        title,
        state,
        tier,
        agent,
        description,
        created_at,
        tags,
        parents,
        children,
        events,
    })
}

fn ticket_ids_in_scope(
    conn: &rusqlite::Connection,
    since_iso: Option<&str>,
    subtree: Option<&HashSet<i64>>,
) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT id FROM task ORDER BY id ASC")?;
    let mut ids: Vec<i64> = stmt
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<Result<_, _>>()?;
    if let Some(set) = subtree {
        ids.retain(|i| set.contains(i));
    }
    if let Some(s) = since_iso {
        // Keep tickets with any event in the window.
        let mut e = conn
            .prepare("SELECT DISTINCT task_id FROM event WHERE task_id IS NOT NULL AND ts >= ?1")?;
        let recent: HashSet<i64> = e
            .query_map([s], |r| r.get::<_, i64>(0))?
            .collect::<Result<_, _>>()?;
        ids.retain(|i| recent.contains(i));
    }
    Ok(ids)
}

// ---------- JSON board payload ----------

fn collect_json(
    conn: &rusqlite::Connection,
    since_iso: Option<&str>,
    subtree: Option<&HashSet<i64>>,
) -> Result<Value> {
    // Tasks: same shape as `qp list --json`.
    let core = store::tasks(conn, &store::TaskFilter::default())?;

    // Filter by subtree if scoped.
    let core: Vec<store::TaskRow> = match subtree {
        Some(set) => core.into_iter().filter(|t| set.contains(&t.id)).collect(),
        None => core,
    };

    let task_ids: Vec<i64> = core.iter().map(|t| t.id).collect();
    let mut tags_by = store::tags_by_task(conn, &task_ids)?;
    let mut blockers_by = store::unresolved_blockers_by_task(conn, &task_ids)?;
    let mut last_event_by = store::last_event_by_task(conn, &task_ids)?;

    let mut tasks: Vec<Value> = Vec::with_capacity(core.len());
    for t in core {
        let mut obj = serde_json::json!({
            "id": t.id, "display_id": t.display_id, "title": t.title, "state": t.state, "tier": t.tier,
            "agent": t.agent,
            "tags": tags_by.remove(&t.id).unwrap_or_default(),
            "blocked_by": blockers_by.remove(&t.id).unwrap_or_default(),
            "last_event": last_event_by.remove(&t.id),
        });
        if let Some(d) = t.description {
            obj.as_object_mut()
                .unwrap()
                .insert("description".into(), Value::String(d));
        }
        tasks.push(obj);
    }

    // Events: same shape as `qp timeline --json`.
    let evt_rows = store::events(conn, &store::EventFilter::default())?;

    let did_to_id: Option<std::collections::HashMap<String, i64>> = if subtree.is_some() {
        let mut m = std::collections::HashMap::new();
        let mut s = conn.prepare("SELECT id, display_id FROM task")?;
        for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, did) = r?;
            m.insert(did, id);
        }
        Some(m)
    } else {
        None
    };

    let mut events: Vec<Value> = Vec::new();
    for e in evt_rows {
        // `--since` scope: skip events before the window.
        if let Some(s) = since_iso {
            if e.ts.as_str() < s {
                continue;
            }
        }
        // Subtree scope: skip events whose task isn't in the subtree.
        if let (Some(ref map), Some(set)) = (&did_to_id, subtree) {
            match e.task.as_ref().and_then(|d| map.get(d)) {
                Some(tid) if set.contains(tid) => {}
                _ => continue,
            }
        }
        events.push(serde_json::json!({
            "id": e.id,
            "task": e.task,
            "ts": e.ts,
            "kind": e.kind,
            "agent_id": e.agent,
            "payload": e.payload,
        }));
        if events.len() >= 200 {
            break;
        }
    }

    // Deps: all dep edges in scope (both tasks must be in subtree if scoped).
    let mut dep_stmt = conn.prepare(
        "SELECT tf.display_id, tt.display_id
           FROM dep d
           JOIN task tf ON tf.id = d.task_id
           JOIN task tt ON tt.id = d.depends_on_task_id
          ORDER BY d.task_id ASC",
    )?;
    let deps_raw: Vec<(String, String)> = dep_stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<_, _>>()?;

    let task_dids: std::collections::HashSet<&str> = tasks
        .iter()
        .filter_map(|t| t["display_id"].as_str())
        .collect();

    let deps: Vec<Value> = deps_raw
        .into_iter()
        .filter(|(from, to)| task_dids.contains(from.as_str()) && task_dids.contains(to.as_str()))
        .map(|(from, to)| serde_json::json!({"from": from, "to": to}))
        .collect();

    Ok(serde_json::json!({
        "tasks": tasks,
        "events": events,
        "deps": deps,
    }))
}

// ---------- duration parsing ----------

/// Parse `--since`. Accepts `Nh`, `Nd`, or an RFC3339-ish date. Returns an
/// RFC3339 UTC string suitable for lexicographic comparison against event.ts.
fn parse_since(s: &str) -> Result<String> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix('h') {
        let hours: i64 = rest
            .parse()
            .map_err(|_| db::invalid_input(format!("bad --since `{s}` (expected Nh)")))?;
        let now = OffsetDateTime::now_utc();
        let then = now - time::Duration::hours(hours);
        return Ok(then.format(&Rfc3339).unwrap());
    }
    if let Some(rest) = s.strip_suffix('d') {
        let days: i64 = rest
            .parse()
            .map_err(|_| db::invalid_input(format!("bad --since `{s}` (expected Nd)")))?;
        let now = OffsetDateTime::now_utc();
        let then = now - time::Duration::days(days);
        return Ok(then.format(&Rfc3339).unwrap());
    }
    // RFC3339 — or a bare YYYY-MM-DD which we widen to T00:00:00Z.
    if s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        return Ok(format!("{s}T00:00:00Z"));
    }
    // Sanity check it actually parses as RFC3339.
    OffsetDateTime::parse(s, &Rfc3339).map_err(|_| {
        db::invalid_input(format!("bad --since `{s}` (expected Nh, Nd, or RFC3339)"))
    })?;
    Ok(s.to_string())
}

fn resolve_subtree(conn: &rusqlite::Connection, task: &str) -> Result<HashSet<i64>> {
    let root = id::resolve_full(conn, task)?.id;
    store::subtree_ids(conn, root)
}
