//! `qp report` — emit a structured JSON snapshot of the qp store.
//!
//! Modes:
//!   default            board payload: `{ tasks, events, deps }`
//!   --ticket <id>      full detail for one ticket (parents, children, uncapped events)
//!   --all-tickets      JSON array of the same per-ticket detail, one entry per ticket in scope
//!
//! Scope filters:
//!   --since <duration>   filter events: `24h`, `7d`, or RFC3339 date
//!   --wave  <task-id>    scope to the dep subtree of the given task
//!
//! Output:
//!   stdout by default, or --output <path> to write to a file.
//!
//! Rendering (markdown/HTML) used to live in this binary; it now lives in the
//! `skills/report-render/` skill — see that skill for the section structure
//! (state snapshot, in-flight, timeline, friction log, open bugs, shipped)
//! that an agent reconstructs from this JSON.

use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde_json::Value;
use std::collections::HashSet;
use std::io::Write;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

/// (id, display_id, title, state, tier, description, agent)
// TODO(QP-68): becomes `store::TaskRow` when the store layer lands.
type TaskCoreRow = (
    i64,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// (event id, task display_id, ts, kind, agent_id, payload)
// TODO(QP-68): becomes `store::EventRow` when the store layer lands.
type EventTailRow = (
    i64,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
);

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
        let tid = id::resolve(&conn, tref)?;
        let detail = collect_ticket(&conn, tid)?;
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
    events: Vec<EventRow>,                  // chronological asc, uncapped
}

/// (task display_id, ts, kind, agent_id, payload)
struct EventRow {
    ts: String,
    kind: String,
    agent: Option<String>,
    payload: Value,
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
    let agent: Option<String> = conn
        .query_row(
            "SELECT agent_id FROM assignment WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
            [tid],
            |r| r.get(0),
        )
        .ok();
    let mut tag_stmt = conn.prepare("SELECT name FROM tag WHERE task_id = ?1 ORDER BY name")?;
    let tags: Vec<String> = tag_stmt
        .query_map([tid], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;

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
    let mut e_stmt = conn.prepare(
        "SELECT e.ts, e.kind, e.agent_id, e.payload
           FROM event e WHERE e.task_id = ?1 ORDER BY e.id ASC",
    )?;
    let events: Vec<EventRow> = e_stmt
        .query_map([tid], |r| {
            let payload: Option<String> = r.get(3)?;
            let payload_v: Value = payload
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .ok()
                .flatten()
                .unwrap_or(Value::Null);
            Ok(EventRow {
                ts: r.get::<_, String>(0)?,
                kind: r.get::<_, String>(1)?,
                agent: r.get::<_, Option<String>>(2)?,
                payload: payload_v,
            })
        })?
        .collect::<Result<_, _>>()?;

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
    let sql = "SELECT t.id, t.display_id, t.title, t.state, t.tier, t.description,
                (SELECT a.agent_id FROM assignment a WHERE a.task_id = t.id ORDER BY a.id DESC LIMIT 1) AS agent
           FROM task t ORDER BY t.id ASC";
    // If subtree-scoped, we filter below (in Rust, not SQL — keeps query simple).
    let mut stmt = conn.prepare(sql)?;
    let core: Vec<TaskCoreRow> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .collect::<Result<_, _>>()?;

    // Filter by subtree if scoped.
    let core: Vec<_> = match subtree {
        Some(set) => core
            .into_iter()
            .filter(|(id, ..)| set.contains(id))
            .collect(),
        None => core,
    };

    let task_ids: Vec<i64> = core.iter().map(|r| r.0).collect();
    let mut tags_by: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
    let mut blockers_by: std::collections::HashMap<i64, Vec<String>> =
        std::collections::HashMap::new();
    let mut last_event_by: std::collections::HashMap<i64, Value> = std::collections::HashMap::new();

    if !task_ids.is_empty() {
        let placeholders = std::iter::repeat_n("?", task_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let pref: Vec<&dyn rusqlite::ToSql> =
            task_ids.iter().map(|i| i as &dyn rusqlite::ToSql).collect();

        let q = format!("SELECT task_id, name FROM tag WHERE task_id IN ({placeholders})");
        let mut s = conn.prepare(&q)?;
        for r in s.query_map(pref.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })? {
            let (t, n) = r?;
            tags_by.entry(t).or_default().push(n);
        }

        let q = format!(
            "SELECT d.task_id, t2.display_id FROM dep d JOIN task t2 ON t2.id = d.depends_on_task_id
              WHERE d.task_id IN ({placeholders}) AND t2.state NOT IN ('done','cancelled')");
        let mut s = conn.prepare(&q)?;
        for r in s.query_map(pref.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })? {
            let (t, d) = r?;
            blockers_by.entry(t).or_default().push(d);
        }

        let q = format!(
            "SELECT task_id, kind, ts, payload FROM event
              WHERE id IN (SELECT MAX(id) FROM event WHERE task_id IN ({placeholders}) GROUP BY task_id)");
        let mut s = conn.prepare(&q)?;
        for r in s.query_map(pref.as_slice(), |r| {
            let payload: Option<String> = r.get(3)?;
            let payload_v: Value = payload.as_deref()
                .map(serde_json::from_str).transpose().ok().flatten().unwrap_or(Value::Null);
            Ok((r.get::<_, i64>(0)?, serde_json::json!({
                "kind": r.get::<_, String>(1)?, "ts": r.get::<_, String>(2)?, "payload": payload_v
            })))
        })? { let (t, v) = r?; last_event_by.insert(t, v); }
    }

    let mut tasks: Vec<Value> = Vec::with_capacity(core.len());
    for (id, did, title, state, tier, description, agent) in core {
        let mut obj = serde_json::json!({
            "id": id, "display_id": did, "title": title, "state": state, "tier": tier,
            "agent": agent,
            "tags": tags_by.remove(&id).unwrap_or_default(),
            "blocked_by": blockers_by.remove(&id).unwrap_or_default(),
            "last_event": last_event_by.remove(&id),
        });
        if let Some(d) = description {
            obj.as_object_mut()
                .unwrap()
                .insert("description".into(), Value::String(d));
        }
        tasks.push(obj);
    }

    // Events: same shape as `qp timeline --json`, scoped + capped at 200.
    let mut evt_sql = String::from(
        "SELECT e.id, t.display_id, e.ts, e.kind, e.agent_id, e.payload
           FROM event e LEFT JOIN task t ON t.id = e.task_id
          WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = since_iso {
        evt_sql.push_str(&format!(" AND e.ts >= ?{}", params.len() + 1));
        params.push(Box::new(s.to_string()));
    }
    evt_sql.push_str(" ORDER BY e.id ASC");
    let mut stmt = conn.prepare(&evt_sql)?;
    let pref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let evt_rows: Vec<EventTailRow> = stmt
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

    // `subtree` is already a set of task rowids — reuse it directly.
    let subtree_task_ids: Option<HashSet<i64>> = subtree.cloned();

    // Subtree scoping for events requires filtering by task rowid; but EventRow only has display_id.
    // We need a lookup from display_id to rowid for subtree filtering.
    // Build it only if needed.
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
    for (eid, did, ts, kind, agent, payload) in evt_rows {
        // Subtree scope: skip events whose task isn't in the subtree.
        if let (Some(ref map), Some(ref set)) = (&did_to_id, &subtree_task_ids) {
            match did.as_ref().and_then(|d| map.get(d)) {
                Some(tid) if set.contains(tid) => {}
                _ => continue,
            }
        }
        let payload_v: Value = payload
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
        events.push(serde_json::json!({
            "id": eid,
            "task": did,
            "ts": ts,
            "kind": kind,
            "agent_id": agent,
            "payload": payload_v,
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
    let root = id::resolve(conn, task)?;
    let mut s = conn.prepare(
        "WITH RECURSIVE sub(id) AS (
            SELECT ?1
            UNION
            SELECT d.depends_on_task_id FROM dep d JOIN sub ON d.task_id = sub.id
         ) SELECT id FROM sub",
    )?;
    let ids: HashSet<i64> = s
        .query_map([root], |r| r.get::<_, i64>(0))?
        .collect::<Result<_, _>>()?;
    Ok(ids)
}
