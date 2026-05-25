//! `qp html` — generate a single self-contained interactive HTML dashboard
//! (filterable task list, SVG DAG, timeline) that can be opened directly in
//! a browser. Uses a meta-refresh tag for near-live updates.

use anyhow::Result;
use clap::Args;
use serde_json::Value;
use std::collections::HashSet;
use std::io::Write;
use crate::{db, id, time as qptime};
use crate::cmd::html_layout::{layout, Layout};
use crate::cmd::html_render::{render, EventData, RenderInput, TaskData};

const MAX_DAG_TASKS: usize = 200;
const MAX_TIMELINE_EVENTS: usize = 200;

#[derive(Args, Debug)]
pub struct HtmlArgs {
    /// Output file path. Default: ./quipu-board.html
    #[arg(long, default_value = "./quipu-board.html")]
    pub output: std::path::PathBuf,
    /// Browser auto-refresh interval in seconds (0 disables). Default: 5.
    #[arg(long, default_value_t = 5)]
    pub refresh: u32,
    /// Scope the dashboard to a wave's dep subtree.
    #[arg(long)]
    pub wave: Option<String>,
}

pub fn run(db_path: &std::path::Path, a: HtmlArgs) -> Result<()> {
    let conn = db::open(db_path)?;

    let subtree = a.wave.as_deref().map(|t| resolve_subtree(&conn, t)).transpose()?;

    // state counts (always full snapshot, even when scoped — matches `qp report`).
    let state_counts = fetch_state_counts(&conn)?;

    // tasks
    let all_tasks = fetch_tasks(&conn)?;
    let tasks: Vec<TaskData> = match &subtree {
        Some(set) => all_tasks.into_iter().filter(|t| set.contains(&t.id)).collect(),
        None => all_tasks,
    };

    // deps for DAG layout
    let deps = fetch_deps(&conn)?;
    let deps_scoped: Vec<(i64, i64)> = match &subtree {
        Some(set) => deps.into_iter().filter(|(a, b)| set.contains(a) && set.contains(b)).collect(),
        None => deps,
    };

    let task_count = tasks.len();
    let svg_too_large = task_count > MAX_DAG_TASKS;

    let layout_input: Vec<(i64, String, String, String)> = tasks.iter()
        .map(|t| (t.id, t.display_id.clone(), t.state.clone(), t.title.clone()))
        .collect();
    let lay: Layout = if svg_too_large {
        Layout { nodes: Vec::new(), edges: Vec::new(), layer_count: 0 }
    } else {
        layout(&layout_input, &deps_scoped)
    };

    // events: last N, scope-filtered
    let events = fetch_events(&conn, subtree.as_ref(), MAX_TIMELINE_EVENTS)?;

    let project = project_name();
    let generated = qptime::now_rfc3339();

    let input = RenderInput {
        project: &project,
        generated_at: &generated,
        state_counts: &state_counts,
        tasks: &tasks,
        events: &events,
        layout: &lay,
        refresh: a.refresh,
        wave: a.wave.as_deref(),
        svg_too_large,
        task_count_for_dag: task_count,
    };
    let html = render(&input);

    let mut f = std::fs::File::create(&a.output)?;
    f.write_all(html.as_bytes())?;
    println!("wrote {}", a.output.display());
    Ok(())
}

fn project_name() -> String {
    std::env::current_dir().ok()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| "quipu".into())
}

fn resolve_subtree(conn: &rusqlite::Connection, task: &str) -> Result<HashSet<i64>> {
    let root = id::resolve(conn, task)?;
    let mut s = conn.prepare(
        "WITH RECURSIVE sub(id) AS (
            SELECT ?1
            UNION
            SELECT d.depends_on_task_id FROM dep d JOIN sub ON d.task_id = sub.id
         ) SELECT id FROM sub")?;
    let ids: HashSet<i64> = s.query_map([root], |r| r.get::<_, i64>(0))?
        .collect::<Result<_, _>>()?;
    Ok(ids)
}

fn fetch_state_counts(conn: &rusqlite::Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare("SELECT state, COUNT(*) FROM task GROUP BY state")?;
    let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for r in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (s, c) = r?; counts.insert(s, c);
    }
    Ok(["pending","ready","assigned","running","done","cancelled"]
        .iter().map(|s| ((*s).to_string(), *counts.get(*s).unwrap_or(&0))).collect())
}

fn fetch_tasks(conn: &rusqlite::Connection) -> Result<Vec<TaskData>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.display_id, t.title, t.state,
                (SELECT a.agent_id FROM assignment a WHERE a.task_id = t.id ORDER BY a.id DESC LIMIT 1)
           FROM task t ORDER BY t.id ASC")?;
    let mut rows: Vec<TaskData> = stmt.query_map([], |r| Ok(TaskData {
        id: r.get(0)?,
        display_id: r.get(1)?,
        title: r.get(2)?,
        state: r.get(3)?,
        agent: r.get(4)?,
        tags: Vec::new(),
    }))?.collect::<Result<_, _>>()?;

    let mut s = conn.prepare("SELECT task_id, name FROM tag ORDER BY name")?;
    let pairs: Vec<(i64, String)> = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (tid, name) in pairs {
        if let Some(row) = rows.iter_mut().find(|r| r.id == tid) {
            row.tags.push(name);
        }
    }
    Ok(rows)
}

fn fetch_deps(conn: &rusqlite::Connection) -> Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare("SELECT task_id, depends_on_task_id FROM dep")?;
    let rows: Vec<(i64, i64)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

fn fetch_events(
    conn: &rusqlite::Connection,
    subtree: Option<&HashSet<i64>>,
    limit: usize,
) -> Result<Vec<EventData>> {
    let mut stmt = conn.prepare(
        "SELECT e.task_id, t.display_id, e.ts, e.kind, e.agent_id, e.payload
           FROM event e LEFT JOIN task t ON t.id = e.task_id
          ORDER BY e.id DESC")?;
    let raw: Vec<(Option<i64>, Option<String>, String, String, Option<String>, Option<String>)> =
        stmt.query_map([], |r| Ok((
            r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
        )))?.collect::<Result<_, _>>()?;

    let mut out = Vec::new();
    for (tid, did, ts, kind, agent, payload) in raw {
        if let Some(set) = subtree {
            match tid {
                Some(t) if set.contains(&t) => {}
                _ => continue,
            }
        }
        let payload_v: Value = payload.as_deref()
            .map(serde_json::from_str).transpose().ok().flatten().unwrap_or(Value::Null);
        out.push(EventData { task: did, ts, kind, agent, payload: payload_v });
        if out.len() >= limit { break; }
    }
    Ok(out)
}
