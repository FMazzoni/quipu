//! `qp report` — emit a structured snapshot of the qp store as Markdown or HTML.
//!
//! Sections (both formats):
//!   1. Header (title, generation timestamp, scope summary)
//!   2. State snapshot (counts across all known states)
//!   3. In flight (ready / assigned / running / pending-with-blockers)
//!   4. Recent timeline (events in scope, newest first, capped at 50)
//!   5. Friction log (decision events with payload.auto == true, rendered body)
//!   6. Open bugs (tasks tagged kind:bug, non-terminal)
//!   7. Recently shipped (done tasks in scope, with commit:<sha> tag if present)
//!
//! Scope filters:
//!   --since <duration>   filter events: `24h`, `7d`, or RFC3339 date
//!   --wave  <task-id>    scope to the dep subtree of the given task
//!
//! Output:
//!   --markdown (default) | --html
//!   --output <path>      write to file instead of stdout

use crate::{db, id, time as qptime};
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

/// As `EventTailRow`, but from a LEFT JOIN where the event id may be NULL.
type EventTailRowOptId = (
    Option<i64>,
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
    /// Emit Markdown (default).
    #[arg(long, conflicts_with = "html", conflicts_with = "json")]
    pub markdown: bool,
    /// Emit a self-contained styled HTML document.
    #[arg(long, conflicts_with = "json")]
    pub html: bool,
    /// Emit a JSON payload for the board (tasks + events + deps).
    #[arg(long)]
    pub json: bool,
    /// Write to this path instead of stdout.
    #[arg(long)]
    pub output: Option<std::path::PathBuf>,
    /// Single-ticket mode: emit a focused report for one ticket.
    #[arg(long, conflicts_with = "all_tickets")]
    pub ticket: Option<String>,
    /// Bulk mode: emit one file per ticket into --output-dir.
    #[arg(
        long = "all-tickets",
        conflicts_with = "ticket",
        requires = "output_dir"
    )]
    pub all_tickets: bool,
    /// Directory to write per-ticket files into (used with --all-tickets).
    #[arg(long = "output-dir")]
    pub output_dir: Option<std::path::PathBuf>,
}

pub fn run(db_path: &std::path::Path, a: ReportArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let since_iso = a.since.as_deref().map(parse_since).transpose()?;
    let subtree = a
        .wave
        .as_deref()
        .map(|t| resolve_subtree(&conn, t))
        .transpose()?;

    // JSON mode: emit board payload.
    if a.json {
        if a.ticket.is_some() || a.all_tickets {
            anyhow::bail!("--json is not compatible with --ticket or --all-tickets");
        }
        let payload = collect_json(&conn, since_iso.as_deref(), subtree.as_ref())?;
        let body = serde_json::to_string(&payload)?;
        if let Some(path) = &a.output {
            let mut f = std::fs::File::create(path)?;
            f.write_all(body.as_bytes())?;
            f.write_all(b"\n")?;
        } else {
            println!("{body}");
        }
        return Ok(());
    }

    // Per-ticket modes.
    if let Some(tref) = a.ticket.as_deref() {
        let tid = id::resolve(&conn, tref)?;
        let detail = collect_ticket(&conn, tid)?;
        let body = if a.html {
            render_ticket_html(&detail)
        } else {
            render_ticket_markdown(&detail)
        };
        if let Some(path) = &a.output {
            let mut f = std::fs::File::create(path)?;
            f.write_all(body.as_bytes())?;
        } else {
            print!("{body}");
            if !body.ends_with('\n') {
                println!();
            }
        }
        return Ok(());
    }
    if a.all_tickets {
        let dir = a
            .output_dir
            .as_ref()
            .ok_or_else(|| db::invalid_input("--all-tickets requires --output-dir"))?;
        std::fs::create_dir_all(dir)?;
        let ext = if a.html { "html" } else { "md" };
        // Iterate tasks, scoped by --wave and (event-based) --since when supplied.
        let scope_ids = ticket_ids_in_scope(&conn, since_iso.as_deref(), subtree.as_ref())?;
        for tid in scope_ids {
            let detail = collect_ticket(&conn, tid)?;
            let slug = slugify(&detail.title);
            let fname = if slug.is_empty() {
                format!("{}.{ext}", detail.display_id)
            } else {
                format!("{}-{}.{ext}", detail.display_id, slug)
            };
            let path = dir.join(fname);
            let body = if a.html {
                render_ticket_html(&detail)
            } else {
                render_ticket_markdown(&detail)
            };
            let mut f = std::fs::File::create(&path)?;
            f.write_all(body.as_bytes())?;
        }
        return Ok(());
    }

    // Default: full snapshot.
    let snap = collect(&conn, since_iso.as_deref(), subtree.as_ref())?;

    let body = if a.html {
        render_html(&snap)
    } else {
        render_markdown(&snap)
    };

    if let Some(path) = &a.output {
        let mut f = std::fs::File::create(path)?;
        f.write_all(body.as_bytes())?;
    } else {
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

// ---------- Per-ticket collection / rendering ----------

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
    events: Vec<EventRow>,                  // chronological asc
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

    // Full timeline for this ticket, oldest-first.
    let mut e_stmt = conn.prepare(
        "SELECT t.display_id, e.ts, e.kind, e.agent_id, e.payload
           FROM event e LEFT JOIN task t ON t.id = e.task_id
          WHERE e.task_id = ?1 ORDER BY e.id ASC",
    )?;
    let events: Vec<EventRow> = e_stmt
        .query_map([tid], |r| {
            let payload: Option<String> = r.get(4)?;
            let payload_v: Value = payload
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .ok()
                .flatten()
                .unwrap_or(Value::Null);
            Ok(EventRow {
                task: r.get::<_, Option<String>>(0)?,
                ts: r.get::<_, String>(1)?,
                kind: r.get::<_, String>(2)?,
                agent: r.get::<_, Option<String>>(3)?,
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

fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut prev_dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 40 {
        out.truncate(40);
        while out.ends_with('-') {
            out.pop();
        }
    }
    out
}

fn render_ticket_markdown(t: &TicketDetail) -> String {
    let mut o = String::new();
    o.push_str(&format!("# {} — {}\n\n", t.display_id, t.title));
    o.push_str(&format!("- **state:** `{}`\n", t.state));
    if let Some(tier) = &t.tier {
        o.push_str(&format!("- **tier:** `{}`\n", tier));
    }
    o.push_str(&format!(
        "- **agent:** {}\n",
        t.agent.as_deref().unwrap_or("—")
    ));
    if let Some(c) = &t.created_at {
        o.push_str(&format!("- **created:** {}\n", c));
    }

    // Tag extraction.
    let mut commit_sha: Option<&str> = None;
    let mut plan: Option<&str> = None;
    let mut critique: Option<&str> = None;
    let mut harness: Option<&str> = None;
    let mut others: Vec<&str> = Vec::new();
    for tag in &t.tags {
        if let Some(v) = tag.strip_prefix("commit:") {
            commit_sha = Some(v);
        } else if let Some(v) = tag.strip_prefix("plan:") {
            plan = Some(v);
        } else if let Some(v) = tag.strip_prefix("critique:") {
            critique = Some(v);
        } else if let Some(v) = tag.strip_prefix("harness:") {
            harness = Some(v);
        } else {
            others.push(tag);
        }
    }
    if let Some(v) = commit_sha {
        o.push_str(&format!("- **commit:** `{}`\n", v));
    }
    if let Some(v) = plan {
        o.push_str(&format!("- **plan:** {}\n", v));
    }
    if let Some(v) = critique {
        o.push_str(&format!("- **critique:** {}\n", v));
    }
    if let Some(v) = harness {
        o.push_str(&format!("- **harness:** {}\n", v));
    }
    if !others.is_empty() {
        o.push_str(&format!("- **tags:** {}\n", others.join(", ")));
    }
    o.push('\n');

    if let Some(d) = t.description.as_deref().filter(|s| !s.is_empty()) {
        o.push_str("## Description\n\n");
        o.push_str(d);
        if !d.ends_with('\n') {
            o.push('\n');
        }
        o.push('\n');
    }

    o.push_str("## Related tickets\n\n");
    if t.parents.is_empty() && t.children.is_empty() {
        o.push_str("_no related tickets_\n\n");
    } else {
        if !t.parents.is_empty() {
            o.push_str("**Depends on:**\n\n");
            for (did, title, state) in &t.parents {
                o.push_str(&format!("- `{}` ({}) {}\n", did, state, md_esc(title)));
            }
            o.push('\n');
        }
        if !t.children.is_empty() {
            o.push_str("**Depended on by:**\n\n");
            for (did, title, state) in &t.children {
                o.push_str(&format!("- `{}` ({}) {}\n", did, state, md_esc(title)));
            }
            o.push('\n');
        }
    }

    o.push_str("## Timeline\n\n");
    if t.events.is_empty() {
        o.push_str("_no events_\n\n");
    } else {
        o.push_str("| ts | kind | agent | summary |\n|----|------|-------|---------|\n");
        for e in &t.events {
            o.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                e.ts,
                e.kind,
                md_esc(e.agent.as_deref().unwrap_or("-")),
                md_esc(&crate::cmd::render::summarize_payload(&e.kind, &e.payload))
            ));
        }
        o.push('\n');
    }
    o
}

fn render_ticket_html(t: &TicketDetail) -> String {
    let mut o = String::new();
    o.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    o.push_str("<meta charset=\"UTF-8\">\n");
    o.push_str(&format!(
        "<title>{} — {}</title>\n",
        html_esc(&t.display_id),
        html_esc(&t.title)
    ));
    o.push_str("<style>\n");
    o.push_str(HTML_CSS);
    o.push_str("</style>\n</head>\n<body><div class=\"wrap\">\n");

    o.push_str(&format!(
        "<h1>{} <span style=\"color:var(--dim);font-weight:400\">— {}</span></h1>\n",
        html_esc(&t.display_id),
        html_esc(&t.title)
    ));
    o.push_str(&format!("<div class=\"subtitle\">state <span class=\"pill p-{0}\">{0}</span> · agent <code>{1}</code></div>\n",
        html_esc(&t.state), html_esc(t.agent.as_deref().unwrap_or("-"))));

    if let Some(d) = t.description.as_deref().filter(|s| !s.is_empty()) {
        o.push_str("<h2>Description</h2>\n");
        o.push_str(&format!(
            "<div class=\"panel\">{}</div>\n",
            html_esc(d).replace('\n', "<br>")
        ));
    }

    if !t.tags.is_empty() {
        o.push_str("<h2>Tags</h2>\n<div class=\"panel\">");
        for tag in &t.tags {
            o.push_str(&format!("<code>{}</code> ", html_esc(tag)));
        }
        o.push_str("</div>\n");
    }

    o.push_str("<h2>Related tickets</h2>\n");
    if t.parents.is_empty() && t.children.is_empty() {
        o.push_str("<div class=\"empty\">no related tickets</div>\n");
    } else {
        if !t.parents.is_empty() {
            o.push_str("<div class=\"panel\"><strong>Depends on:</strong><ul>\n");
            for (did, title, state) in &t.parents {
                o.push_str(&format!("<li><span class=\"id\">{}</span> <span class=\"pill p-{1}\">{1}</span> {2}</li>\n",
                    html_esc(did), html_esc(state), html_esc(title)));
            }
            o.push_str("</ul></div>\n");
        }
        if !t.children.is_empty() {
            o.push_str("<div class=\"panel\"><strong>Depended on by:</strong><ul>\n");
            for (did, title, state) in &t.children {
                o.push_str(&format!("<li><span class=\"id\">{}</span> <span class=\"pill p-{1}\">{1}</span> {2}</li>\n",
                    html_esc(did), html_esc(state), html_esc(title)));
            }
            o.push_str("</ul></div>\n");
        }
    }

    o.push_str("<h2>Timeline</h2>\n");
    if t.events.is_empty() {
        o.push_str("<div class=\"empty\">no events</div>\n");
    } else {
        o.push_str("<table><thead><tr><th>ts</th><th>kind</th><th>agent</th><th>summary</th></tr></thead><tbody>\n");
        for e in &t.events {
            o.push_str(&format!(
                "<tr><td class=\"ts\">{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td>{}</td></tr>\n",
                html_esc(&e.ts), html_esc(&e.kind),
                html_esc(e.agent.as_deref().unwrap_or("-")),
                html_esc(&crate::cmd::render::summarize_payload(&e.kind, &e.payload))));
        }
        o.push_str("</tbody></table>\n");
    }

    o.push_str("<div class=\"footer\">qp report --ticket</div>\n");
    o.push_str("</div></body></html>\n");
    o
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

// ---------- data collection ----------

struct TaskRow {
    #[allow(dead_code)]
    id: i64,
    display_id: String,
    title: String,
    state: String,
    agent: Option<String>,
    tags: Vec<String>,
    commit_sha: Option<String>,
    blockers: Vec<String>,
}

struct EventRow {
    task: Option<String>,
    ts: String,
    kind: String,
    agent: Option<String>,
    payload: Value,
}

struct Snapshot {
    generated_at: String,
    scope_since: Option<String>,
    scope_wave: Option<String>,
    state_counts: Vec<(String, i64)>,
    in_flight: Vec<TaskRow>, // ready / assigned / running / pending-with-blockers
    timeline: Vec<EventRow>, // newest first, capped 50
    timeline_truncated: bool,
    friction: Vec<EventRow>, // decision + auto, newest first
    open_bugs: Vec<TaskRow>,
    shipped: Vec<TaskRow>,
}

fn collect(
    conn: &rusqlite::Connection,
    since_iso: Option<&str>,
    subtree: Option<&HashSet<i64>>,
) -> Result<Snapshot> {
    // state counts (always full, not scoped — global view)
    let mut stmt =
        conn.prepare("SELECT state, COUNT(*) FROM task GROUP BY state ORDER BY state")?;
    let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for r in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (s, c) = r?;
        counts.insert(s, c);
    }
    for s in [
        "pending",
        "ready",
        "assigned",
        "running",
        "done",
        "cancelled",
    ] {
        counts.entry(s.to_string()).or_insert(0);
    }
    let state_counts: Vec<(String, i64)> = [
        "pending",
        "ready",
        "assigned",
        "running",
        "done",
        "cancelled",
    ]
    .iter()
    .map(|s| ((*s).to_string(), *counts.get(*s).unwrap_or(&0)))
    .collect();

    // all tasks, then filter by subtree if scoped
    let all_tasks = fetch_all_tasks(conn)?;
    let scoped: Vec<&TaskRow> = match subtree {
        Some(set) => all_tasks.iter().filter(|t| set.contains(&t.id)).collect(),
        None => all_tasks.iter().collect(),
    };

    // In-flight: ready/assigned/running plus pending-with-blockers
    let mut in_flight: Vec<TaskRow> = Vec::new();
    for t in &scoped {
        match t.state.as_str() {
            "ready" | "assigned" | "running" => in_flight.push(clone_row(t)),
            "pending" if !t.blockers.is_empty() => in_flight.push(clone_row(t)),
            _ => {}
        }
    }

    // Timeline: scoped events
    let timeline_all = fetch_events(conn, since_iso, subtree, None)?;
    let timeline_truncated = timeline_all.len() > 50;
    let mut timeline = timeline_all;
    timeline.truncate(50);

    // Friction: decision + auto
    let friction = fetch_events(conn, since_iso, subtree, Some("decision"))?
        .into_iter()
        .filter(|e| {
            e.payload
                .get("auto")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .collect();

    // Open bugs: tag = kind:bug, non-terminal, scope-filtered
    let open_bugs: Vec<TaskRow> = scoped
        .iter()
        .filter(|t| t.tags.iter().any(|n| n == "kind:bug"))
        .filter(|t| t.state != "done" && t.state != "cancelled")
        .map(|t| clone_row(t))
        .collect();

    // Shipped: done, scope-filtered. Optional --since filter via last state_change event.
    let mut shipped: Vec<TaskRow> = scoped
        .iter()
        .filter(|t| t.state == "done")
        .map(|t| clone_row(t))
        .collect();
    if let Some(since) = since_iso {
        // Keep only tasks whose latest state_change to 'done' falls in the window.
        let cutoff_ids = events_in_window_for_kind(conn, since, "state_change")?;
        shipped.retain(|t| cutoff_ids.contains(&t.id));
    }

    Ok(Snapshot {
        generated_at: qptime::now_rfc3339(),
        scope_since: since_iso.map(String::from),
        scope_wave: subtree.map(|_| String::new()).map(|_| {
            // we don't have the original token here; collect_scope_label gets it in run()
            String::from("scoped")
        }),
        state_counts,
        in_flight,
        timeline,
        timeline_truncated,
        friction,
        open_bugs,
        shipped,
    })
}

fn clone_row(t: &TaskRow) -> TaskRow {
    TaskRow {
        id: t.id,
        display_id: t.display_id.clone(),
        title: t.title.clone(),
        state: t.state.clone(),
        agent: t.agent.clone(),
        tags: t.tags.clone(),
        commit_sha: t.commit_sha.clone(),
        blockers: t.blockers.clone(),
    }
}

fn fetch_all_tasks(conn: &rusqlite::Connection) -> Result<Vec<TaskRow>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.display_id, t.title, t.state,
                (SELECT a.agent_id FROM assignment a WHERE a.task_id = t.id ORDER BY a.id DESC LIMIT 1)
           FROM task t ORDER BY t.id ASC")?;
    let mut rows: Vec<TaskRow> = stmt
        .query_map([], |r| {
            Ok(TaskRow {
                id: r.get(0)?,
                display_id: r.get(1)?,
                title: r.get(2)?,
                state: r.get(3)?,
                agent: r.get(4)?,
                tags: Vec::new(),
                commit_sha: None,
                blockers: Vec::new(),
            })
        })?
        .collect::<Result<_, _>>()?;

    // tags
    let mut s = conn.prepare("SELECT task_id, name FROM tag")?;
    let pairs: Vec<(i64, String)> = s
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (tid, name) in pairs {
        if let Some(row) = rows.iter_mut().find(|r| r.id == tid) {
            if let Some(sha) = name.strip_prefix("commit:") {
                row.commit_sha = Some(sha.to_string());
            }
            row.tags.push(name);
        }
    }

    // unresolved blockers (for pending tasks specifically — but compute for all, cheap)
    let mut s = conn.prepare(
        "SELECT d.task_id, t2.display_id
           FROM dep d JOIN task t2 ON t2.id = d.depends_on_task_id
          WHERE t2.state NOT IN ('done','cancelled')",
    )?;
    let pairs: Vec<(i64, String)> = s
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (tid, dep) in pairs {
        if let Some(row) = rows.iter_mut().find(|r| r.id == tid) {
            row.blockers.push(dep);
        }
    }

    Ok(rows)
}

fn fetch_events(
    conn: &rusqlite::Connection,
    since_iso: Option<&str>,
    subtree: Option<&HashSet<i64>>,
    kind_filter: Option<&str>,
) -> Result<Vec<EventRow>> {
    let mut sql = String::from(
        "SELECT e.task_id, t.display_id, e.ts, e.kind, e.agent_id, e.payload
           FROM event e LEFT JOIN task t ON t.id = e.task_id
          WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = since_iso {
        sql.push_str(&format!(" AND e.ts >= ?{}", params.len() + 1));
        params.push(Box::new(s.to_string()));
    }
    if let Some(k) = kind_filter {
        sql.push_str(&format!(" AND e.kind = ?{}", params.len() + 1));
        params.push(Box::new(k.to_string()));
    }
    sql.push_str(" ORDER BY e.id DESC");
    let mut stmt = conn.prepare(&sql)?;
    let pref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows: Vec<EventTailRowOptId> = stmt
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

    let mut out = Vec::with_capacity(rows.len());
    for (tid, did, ts, kind, agent, payload) in rows {
        if let Some(set) = subtree {
            match tid {
                Some(t) if set.contains(&t) => {}
                _ => continue, // events without a task, or outside subtree, are dropped in scoped mode
            }
        }
        let payload_v: Value = payload
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
        out.push(EventRow {
            task: did,
            ts,
            kind,
            agent,
            payload: payload_v,
        });
    }
    Ok(out)
}

/// Return task_ids that had a state_change to 'done' at or after `since_iso`.
fn events_in_window_for_kind(
    conn: &rusqlite::Connection,
    since_iso: &str,
    kind: &str,
) -> Result<HashSet<i64>> {
    let mut s = conn.prepare(
        "SELECT DISTINCT task_id FROM event
          WHERE kind = ?1 AND ts >= ?2 AND task_id IS NOT NULL
            AND json_extract(payload, '$.to') = 'done'",
    )?;
    let ids: HashSet<i64> = s
        .query_map(rusqlite::params![kind, since_iso], |r| r.get::<_, i64>(0))?
        .collect::<Result<_, _>>()?;
    Ok(ids)
}

// ---------- Markdown renderer ----------

fn render_markdown(s: &Snapshot) -> String {
    let mut o = String::new();
    o.push_str("# quipu report\n\n");
    o.push_str(&format!("_generated {} UTC_\n\n", s.generated_at));
    if let Some(since) = &s.scope_since {
        o.push_str(&format!("**Scope:** events since `{since}`"));
        if s.scope_wave.is_some() {
            o.push_str(" · wave subtree");
        }
        o.push_str("\n\n");
    } else if s.scope_wave.is_some() {
        o.push_str("**Scope:** wave subtree\n\n");
    }

    // 2. State snapshot
    o.push_str("## State snapshot\n\n");
    o.push_str("| state | count |\n|-------|------:|\n");
    for (st, c) in &s.state_counts {
        o.push_str(&format!("| {st} | {c} |\n"));
    }
    o.push('\n');

    // 3. In flight
    o.push_str("## In flight\n\n");
    if s.in_flight.is_empty() {
        o.push_str("_nothing in flight_\n\n");
    } else {
        o.push_str("| id | state | agent | blockers | title |\n|----|-------|-------|----------|-------|\n");
        for t in &s.in_flight {
            let agent = t.agent.as_deref().unwrap_or("-");
            let blk = if t.blockers.is_empty() {
                "-".into()
            } else {
                t.blockers.join(",")
            };
            o.push_str(&format!(
                "| `{}` | {} | {} | {} | {} |\n",
                t.display_id,
                t.state,
                md_esc(agent),
                md_esc(&blk),
                md_esc(&t.title)
            ));
        }
        o.push('\n');
    }

    // 4. Recent timeline
    o.push_str("## Recent timeline\n\n");
    if s.timeline.is_empty() {
        o.push_str("_no events_\n\n");
    } else {
        o.push_str(
            "| ts | task | kind | agent | summary |\n|----|------|------|-------|---------|\n",
        );
        for e in &s.timeline {
            o.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                e.ts,
                e.task.as_deref().unwrap_or("-"),
                e.kind,
                md_esc(e.agent.as_deref().unwrap_or("-")),
                md_esc(&crate::cmd::render::summarize_payload(&e.kind, &e.payload))
            ));
        }
        if s.timeline_truncated {
            o.push_str("\n_capped at 50 events; use `--since` to narrow_\n");
        }
        o.push('\n');
    }

    // 5. Friction log
    o.push_str("## Friction log\n\n");
    if s.friction.is_empty() {
        o.push_str("_no auto-decisions in scope_\n\n");
    } else {
        for e in &s.friction {
            let text = e.payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
            o.push_str(&format!(
                "- **{}** · `{}` · {}: {}\n",
                e.ts,
                e.task.as_deref().unwrap_or("-"),
                md_esc(e.agent.as_deref().unwrap_or("-")),
                md_esc(text)
            ));
        }
        o.push('\n');
    }

    // 6. Open bugs
    o.push_str("## Open bugs\n\n");
    if s.open_bugs.is_empty() {
        o.push_str("_no open bugs_\n\n");
    } else {
        o.push_str("| id | state | agent | title |\n|----|-------|-------|-------|\n");
        for t in &s.open_bugs {
            o.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                t.display_id,
                t.state,
                md_esc(t.agent.as_deref().unwrap_or("-")),
                md_esc(&t.title)
            ));
        }
        o.push('\n');
    }

    // 7. Recently shipped
    o.push_str("## Recently shipped\n\n");
    if s.shipped.is_empty() {
        o.push_str("_nothing shipped in scope_\n\n");
    } else {
        o.push_str("| id | commit | title |\n|----|--------|-------|\n");
        for t in &s.shipped {
            let sha = t.commit_sha.as_deref().unwrap_or("-");
            o.push_str(&format!(
                "| `{}` | `{}` | {} |\n",
                t.display_id,
                sha,
                md_esc(&t.title)
            ));
        }
        o.push('\n');
    }
    o
}

fn md_esc(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

// ---------- HTML renderer ----------

const HTML_CSS: &str = r#":root{
  --bg:#0f1419; --panel:#1a1f29; --panel-2:#232936; --border:#2d3548;
  --text:#d4d4dc; --dim:#7a8499; --accent:#6cb6ff;
  --ok:#69d68a; --warn:#ffb347; --bad:#ff6e6e;
  --s-pending:#6b7280; --s-ready:#6cb6ff; --s-assigned:#ffb347;
  --s-running:#69d68a; --s-done:#2d8a4c; --s-cancelled:#4a4f5c;
}
*{box-sizing:border-box}
body{margin:0;padding:28px 20px;
  font:14px/1.55 -apple-system,BlinkMacSystemFont,"SF Pro Text",system-ui,sans-serif;
  background:var(--bg);color:var(--text)}
.wrap{max-width:1100px;margin:0 auto}
h1{font-size:26px;margin:0 0 4px;letter-spacing:-0.01em}
h2{font-size:12px;margin:32px 0 12px;color:var(--accent);
  text-transform:uppercase;letter-spacing:0.08em;
  padding-bottom:8px;border-bottom:1px solid var(--border)}
.subtitle{color:var(--dim);max-width:720px;margin-bottom:8px}
code,.mono{font-family:ui-monospace,"SF Mono",Menlo,monospace}
code{background:var(--panel-2);padding:1px 6px;border-radius:3px;font-size:12.5px}
.panel{background:var(--panel);border:1px solid var(--border);border-radius:10px;
  padding:18px;margin-bottom:14px}
.hero{display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:10px;margin:14px 0}
.stat{background:var(--panel-2);border:1px solid var(--border);border-radius:8px;padding:12px 14px}
.stat .label{color:var(--dim);font-size:11px;text-transform:uppercase;letter-spacing:0.06em}
.stat .val{font:600 22px ui-monospace,monospace;color:var(--text);margin-top:4px}
table{width:100%;border-collapse:collapse;font-size:13px;margin:8px 0}
th,td{padding:6px 10px;text-align:left;border-bottom:1px solid var(--border);vertical-align:top}
th{color:var(--dim);font-weight:500;font-size:11px;text-transform:uppercase;letter-spacing:0.05em}
tr:hover td{background:var(--panel-2)}
.pill{display:inline-block;padding:1px 7px;border-radius:3px;font:600 11px ui-monospace,monospace}
.p-pending{background:rgba(107,114,128,.2);color:var(--s-pending)}
.p-ready{background:rgba(108,182,255,.15);color:var(--s-ready)}
.p-assigned{background:rgba(255,179,71,.15);color:var(--s-assigned)}
.p-running{background:rgba(105,214,138,.15);color:var(--s-running)}
.p-done{background:rgba(45,138,76,.2);color:var(--s-done)}
.p-cancelled{background:rgba(74,79,92,.3);color:var(--s-cancelled)}
.id{font:600 12.5px ui-monospace,monospace;color:var(--accent)}
.ts{font:12px ui-monospace,monospace;color:var(--dim);white-space:nowrap}
.empty{color:var(--dim);font-style:italic;padding:6px 0}
.friction{background:var(--panel-2);border-left:3px solid var(--accent);
  padding:10px 14px;margin:6px 0;border-radius:0 6px 6px 0}
.friction .meta{font:11.5px ui-monospace,monospace;color:var(--dim);margin-bottom:4px}
.footer{margin-top:36px;padding-top:14px;border-top:1px solid var(--border);
  color:var(--dim);font-size:12px;text-align:center}
"#;

fn render_html(s: &Snapshot) -> String {
    let mut o = String::new();
    o.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    o.push_str("<meta charset=\"UTF-8\">\n");
    o.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    o.push_str("<title>quipu report</title>\n<style>\n");
    o.push_str(HTML_CSS);
    o.push_str("</style>\n</head>\n<body><div class=\"wrap\">\n");

    // header
    o.push_str("<h1>quipu report</h1>\n");
    o.push_str(&format!(
        "<div class=\"subtitle\">generated {} UTC",
        html_esc(&s.generated_at)
    ));
    if let Some(since) = &s.scope_since {
        o.push_str(&format!(" · events since <code>{}</code>", html_esc(since)));
    }
    if s.scope_wave.is_some() {
        o.push_str(" · wave subtree");
    }
    o.push_str("</div>\n");

    // state snapshot — hero cards
    o.push_str("<h2>State snapshot</h2>\n<div class=\"hero\">\n");
    for (st, c) in &s.state_counts {
        o.push_str(&format!(
            "<div class=\"stat\"><div class=\"label\">{}</div><div class=\"val\">{}</div></div>\n",
            html_esc(st),
            c
        ));
    }
    o.push_str("</div>\n");

    // in flight
    o.push_str("<h2>In flight</h2>\n");
    if s.in_flight.is_empty() {
        o.push_str("<div class=\"empty\">nothing in flight</div>\n");
    } else {
        o.push_str("<table><thead><tr><th>id</th><th>state</th><th>agent</th><th>blockers</th><th>title</th></tr></thead><tbody>\n");
        for t in &s.in_flight {
            let blk = if t.blockers.is_empty() {
                "-".into()
            } else {
                t.blockers.join(",")
            };
            o.push_str(&format!(
                "<tr><td><span class=\"id\">{}</span></td><td><span class=\"pill p-{}\">{}</span></td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td>{}</td></tr>\n",
                html_esc(&t.display_id), html_esc(&t.state), html_esc(&t.state),
                html_esc(t.agent.as_deref().unwrap_or("-")),
                html_esc(&blk),
                html_esc(&t.title)));
        }
        o.push_str("</tbody></table>\n");
    }

    // recent timeline
    o.push_str("<h2>Recent timeline</h2>\n");
    if s.timeline.is_empty() {
        o.push_str("<div class=\"empty\">no events</div>\n");
    } else {
        o.push_str("<table><thead><tr><th>ts</th><th>task</th><th>kind</th><th>agent</th><th>summary</th></tr></thead><tbody>\n");
        for e in &s.timeline {
            o.push_str(&format!(
                "<tr><td class=\"ts\">{}</td><td><span class=\"id\">{}</span></td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td>{}</td></tr>\n",
                html_esc(&e.ts),
                html_esc(e.task.as_deref().unwrap_or("-")),
                html_esc(&e.kind),
                html_esc(e.agent.as_deref().unwrap_or("-")),
                html_esc(&crate::cmd::render::summarize_payload(&e.kind, &e.payload))));
        }
        o.push_str("</tbody></table>\n");
        if s.timeline_truncated {
            o.push_str("<div class=\"empty\">capped at 50 events; use <code>--since</code> to narrow</div>\n");
        }
    }

    // friction
    o.push_str("<h2>Friction log</h2>\n");
    if s.friction.is_empty() {
        o.push_str("<div class=\"empty\">no auto-decisions in scope</div>\n");
    } else {
        for e in &s.friction {
            let text = e.payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
            o.push_str(&format!(
                "<div class=\"friction\"><div class=\"meta\">{} · <span class=\"id\">{}</span> · {}</div>{}</div>\n",
                html_esc(&e.ts),
                html_esc(e.task.as_deref().unwrap_or("-")),
                html_esc(e.agent.as_deref().unwrap_or("-")),
                html_esc(text)));
        }
    }

    // open bugs
    o.push_str("<h2>Open bugs</h2>\n");
    if s.open_bugs.is_empty() {
        o.push_str("<div class=\"empty\">no open bugs</div>\n");
    } else {
        o.push_str("<table><thead><tr><th>id</th><th>state</th><th>agent</th><th>title</th></tr></thead><tbody>\n");
        for t in &s.open_bugs {
            o.push_str(&format!(
                "<tr><td><span class=\"id\">{}</span></td><td><span class=\"pill p-{}\">{}</span></td><td class=\"mono\">{}</td><td>{}</td></tr>\n",
                html_esc(&t.display_id), html_esc(&t.state), html_esc(&t.state),
                html_esc(t.agent.as_deref().unwrap_or("-")),
                html_esc(&t.title)));
        }
        o.push_str("</tbody></table>\n");
    }

    // shipped
    o.push_str("<h2>Recently shipped</h2>\n");
    if s.shipped.is_empty() {
        o.push_str("<div class=\"empty\">nothing shipped in scope</div>\n");
    } else {
        o.push_str(
            "<table><thead><tr><th>id</th><th>commit</th><th>title</th></tr></thead><tbody>\n",
        );
        for t in &s.shipped {
            let sha = t.commit_sha.as_deref().unwrap_or("-");
            o.push_str(&format!(
                "<tr><td><span class=\"id\">{}</span></td><td class=\"mono\">{}</td><td>{}</td></tr>\n",
                html_esc(&t.display_id), html_esc(sha), html_esc(&t.title)));
        }
        o.push_str("</tbody></table>\n");
    }

    o.push_str("<div class=\"footer\">qp report</div>\n");
    o.push_str("</div></body></html>\n");
    o
}

fn html_esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}
