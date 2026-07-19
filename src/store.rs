//! Canonical queries over the qp schema.
//!
#![doc = include_str!("../docs/modules/store.md")]
#![allow(dead_code)] // populated incrementally; some helpers land before their callers

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, ToSql};
use std::collections::{HashMap, HashSet};
/// One row of the `event LEFT JOIN task` tail, payload already parsed.
///
/// `task` is `Option` because the join is a `LEFT` join — a store-scoped event
/// carries no task. `payload` is never an error: absent and malformed JSON
/// both degrade to `Null`, so a single bad row cannot fail a whole timeline
/// read. The cost of that choice is that the two cases are indistinguishable
/// downstream; a consumer that must tell them apart has to read the raw column
/// itself.
pub struct EventRow {
    pub id: i64,
    pub task: Option<String>, // display_id
    pub ts: String,
    pub kind: String,
    pub agent: Option<String>,
    pub payload: serde_json::Value, // parsed; malformed/absent payload -> Null
}

/// Filter for [`events`].
///
/// All fields are conjunctive (`AND`); `None`/empty means "no constraint on
/// this dimension".
///
/// This is the smallest shape covering all three event-tail callers —
/// `timeline.rs`, `watch.rs`, and `decisions.rs` — which previously hand-rolled
/// the same `event LEFT JOIN task` SELECT with different `?N` numbering.
/// Every dimension here exists because some caller needs it: `decisions` alone
/// sets `auto_only` and passes a single-element `kinds` slice, and it alone
/// passes `since_id: None` because it wants full history rather than a tail.
///
/// There is no sort-direction field, deliberately: all three callers order
/// `e.id ASC`, so an `ascending` flag would be dead weight on every existing
/// call site. Add one when a caller needs it, not before.
#[derive(Default)]
pub struct EventFilter<'a> {
    /// `e.id > since_id`. `None` omits the clause entirely (decisions wants
    /// full history, not "since event 0" — same result today since ids start
    /// at 1, but `None` is the honest way to say "no lower bound").
    pub since_id: Option<i64>,
    /// `e.task_id = task_id`.
    pub task_id: Option<i64>,
    /// `e.kind IN (...)`. Empty slice omits the clause.
    pub kinds: &'a [String],
    /// Adds `e.payload IS NOT NULL AND json_extract(e.payload, '$.auto') = 1`.
    pub auto_only: bool,
}

/// The event tail: `event LEFT JOIN task`, filtered by `f`, ordered `e.id ASC`.
///
/// Bound parameters throughout — no string-interpolated values.
pub fn events(conn: &rusqlite::Connection, f: &EventFilter) -> anyhow::Result<Vec<EventRow>> {
    let mut sql = String::from(
        "SELECT e.id, t.display_id, e.ts, e.kind, e.agent_id, e.payload
           FROM event e LEFT JOIN task t ON t.id = e.task_id",
    );
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(since_id) = f.since_id {
        params.push(Box::new(since_id));
        clauses.push(format!("e.id > ?{}", params.len()));
    }
    if let Some(task_id) = f.task_id {
        params.push(Box::new(task_id));
        clauses.push(format!("e.task_id = ?{}", params.len()));
    }
    if !f.kinds.is_empty() {
        let placeholders: Vec<String> = f
            .kinds
            .iter()
            .map(|k| {
                params.push(Box::new(k.clone()));
                format!("?{}", params.len())
            })
            .collect();
        clauses.push(format!("e.kind IN ({})", placeholders.join(",")));
    }
    if f.auto_only {
        clauses.push("e.payload IS NOT NULL AND json_extract(e.payload, '$.auto') = 1".to_string());
    }

    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    sql.push_str(" ORDER BY e.id ASC");

    let mut stmt = conn.prepare(&sql)?;
    let pref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(pref.as_slice(), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (id, task, ts, kind, agent, payload) = row?;
        // Malformed/absent payload legitimately degrades to Null (out of
        // scope for the QP-71 .ok() sweep — different class of error).
        let payload_v: serde_json::Value = payload
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .ok()
            .flatten()
            .unwrap_or(serde_json::Value::Null);
        out.push(EventRow {
            id,
            task,
            ts,
            kind,
            agent,
            payload: payload_v,
        });
    }
    Ok(out)
}

/// A task row as read by the list/tree/wave/show query families.
///
/// `agent` is the *latest* agent assigned, open or not (see
/// `LATEST_AGENT_SUBQUERY`) — not necessarily the agent that holds the task
/// now. A `done` task keeps naming whoever finished it, which is what the list
/// and wave views want to show, but it means a non-null `agent` here is not
/// evidence of an open claim. Use `db::current_assignment` for ownership
/// questions.
///
/// `state` stays a plain `String` for now — typing it as `db::State` is QP-66,
/// a separate ticket; conflating query extraction with column retyping would
/// double the review surface for no immediate benefit.
pub struct TaskRow {
    pub id: i64,
    pub display_id: String,
    pub title: String,
    pub state: String,
    pub tier: Option<String>,
    pub description: Option<String>,
    pub agent: Option<String>,
}

/// Filter for `tasks`.
///
/// Every field is optional/empty-by-default so call sites only populate what
/// they actually filter on (`tree.rs` uses only `tier`; `list.rs` uses all
/// four).
///
/// The two free-text predicates — `assigned_to_glob` and `tag_globs` — both
/// match with SQLite `GLOB`, deliberately and identically. A pattern with no
/// wildcard degrades to exact match, so globbing is a strict superset of the
/// old `=` behaviour for every input that is not itself glob syntax. The
/// consequence worth knowing before "fixing" this: `[` and `]` are
/// character-class metacharacters, so an agent_id or tag containing literal
/// brackets cannot be matched by pasting it in verbatim (QP-41, closed as
/// accepted). Escaping brackets was rejected — SQLite `GLOB` has no escape
/// syntax, so honouring literals means rewriting the pattern into a
/// `LIKE`/`instr` hybrid and giving the two predicates different matching
/// languages. One language applied uniformly beats two dialects.
#[derive(Default)]
pub struct TaskFilter<'a> {
    pub state: Option<&'a str>,
    pub assigned_to_glob: Option<&'a str>,
    pub tag_globs: &'a [String],
    pub tier: Option<&'a str>,
}

/// The "latest agent for a task" correlated subquery.
///
/// The most recent assignment row by id, regardless of whether it's still
/// open. This is distinct from `db::current_assignment`, which filters to open
/// (`completed_at IS NULL`) assignments only — the two answer different
/// questions ("who was last assigned" vs "who currently holds it") and are not
/// interchangeable. Embedded byte-identically across `list.rs` (x2) and
/// `wave.rs` (x4) prior to this extraction.
pub const LATEST_AGENT_SUBQUERY: &str =
    "(SELECT a.agent_id FROM assignment a WHERE a.task_id = t.id ORDER BY a.id DESC LIMIT 1)";

/// Core task query, filtered per `f`. Ordered by `t.id ASC`.
pub fn tasks(conn: &Connection, f: &TaskFilter) -> Result<Vec<TaskRow>> {
    let mut sql = format!(
        "SELECT t.id, t.display_id, t.title, t.state, t.tier, t.description,
                {agent} AS agent
           FROM task t WHERE 1=1",
        agent = LATEST_AGENT_SUBQUERY
    );
    let mut params: Vec<Box<dyn ToSql>> = Vec::new();
    if let Some(s) = f.state {
        sql.push_str(" AND t.state = ?");
        params.push(Box::new(s.to_string()));
    }
    if let Some(who) = f.assigned_to_glob {
        sql.push_str(&format!(
            " AND {agent} GLOB ?",
            agent = LATEST_AGENT_SUBQUERY
        ));
        params.push(Box::new(who.to_string()));
    }
    for tag in f.tag_globs {
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM tag WHERE tag.task_id = t.id AND tag.name GLOB ?)",
        );
        params.push(Box::new(tag.clone()));
    }
    if let Some(tier) = f.tier {
        sql.push_str(" AND t.tier = ?");
        params.push(Box::new(tier.to_string()));
    }
    sql.push_str(" ORDER BY t.id ASC");

    let mut stmt = conn.prepare(&sql)?;
    let pref: Vec<&dyn ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(pref.as_slice(), |r| {
            Ok(TaskRow {
                id: r.get(0)?,
                display_id: r.get(1)?,
                title: r.get(2)?,
                state: r.get(3)?,
                tier: r.get(4)?,
                description: r.get(5)?,
                agent: r.get(6)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// The latest agent assigned to a single task, open or not.
///
/// Standalone-query counterpart to `LATEST_AGENT_SUBQUERY` for call sites that
/// already have one task in hand (e.g. `show.rs`) rather than embedding it in
/// a larger SELECT.
pub fn latest_agent(conn: &Connection, task_id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT agent_id FROM assignment WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
        [task_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

/// SQLite's variable limit (32,766 in the modern amalgamation this crate
/// bundles) — chunk bulk `IN (...)` lookups under it. Theoretical at current
/// task-count scales, but the bulk helpers below make chunking free.
const SQL_VAR_CHUNK: usize = 32_000;

fn placeholders(n: usize) -> String {
    std::iter::repeat_n("?", n).collect::<Vec<_>>().join(",")
}

/// Tags for each of `ids`, bulk-fetched.
pub fn tags_by_task(conn: &Connection, ids: &[i64]) -> Result<HashMap<i64, Vec<String>>> {
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    for chunk in ids.chunks(SQL_VAR_CHUNK) {
        if chunk.is_empty() {
            continue;
        }
        let q = format!(
            "SELECT task_id, name FROM tag WHERE task_id IN ({})",
            placeholders(chunk.len())
        );
        let mut s = conn.prepare(&q)?;
        let pref: Vec<&dyn ToSql> = chunk.iter().map(|i| i as &dyn ToSql).collect();
        for r in s.query_map(pref.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })? {
            let (t, n) = r?;
            out.entry(t).or_default().push(n);
        }
    }
    Ok(out)
}

/// Unresolved dependency display-ids for each of `ids`, bulk-fetched.
///
/// Unresolved means not `done` and not `cancelled`. This is the read-side form
/// of the unresolved-dep predicate that also appears (as a guarded-transition
/// UPDATE/EXISTS check, out of scope here) in `db::refresh_ready` and
/// `depends.rs`.
pub fn unresolved_blockers_by_task(
    conn: &Connection,
    ids: &[i64],
) -> Result<HashMap<i64, Vec<String>>> {
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    for chunk in ids.chunks(SQL_VAR_CHUNK) {
        if chunk.is_empty() {
            continue;
        }
        let q = format!(
            "SELECT d.task_id, t2.display_id
               FROM dep d JOIN task t2 ON t2.id = d.depends_on_task_id
              WHERE d.task_id IN ({}) AND t2.state NOT IN ('done','cancelled')
              ORDER BY t2.id",
            placeholders(chunk.len())
        );
        let mut s = conn.prepare(&q)?;
        let pref: Vec<&dyn ToSql> = chunk.iter().map(|i| i as &dyn ToSql).collect();
        for r in s.query_map(pref.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })? {
            let (t, d) = r?;
            out.entry(t).or_default().push(d);
        }
    }
    Ok(out)
}

/// The most recent event for each of `ids`, as `{"kind", "ts", "payload"}`.
pub fn last_event_by_task(
    conn: &Connection,
    ids: &[i64],
) -> Result<HashMap<i64, serde_json::Value>> {
    let mut out: HashMap<i64, serde_json::Value> = HashMap::new();
    for chunk in ids.chunks(SQL_VAR_CHUNK) {
        if chunk.is_empty() {
            continue;
        }
        let ph = placeholders(chunk.len());
        let q = format!(
            "SELECT task_id, kind, ts, payload FROM event
              WHERE id IN (SELECT MAX(id) FROM event WHERE task_id IN ({ph}) GROUP BY task_id)"
        );
        let mut s = conn.prepare(&q)?;
        let pref: Vec<&dyn ToSql> = chunk.iter().map(|i| i as &dyn ToSql).collect();
        for r in s.query_map(pref.as_slice(), |r| {
            let payload: Option<String> = r.get(3)?;
            let payload_v: serde_json::Value = payload
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .ok()
                .flatten()
                .unwrap_or(serde_json::Value::Null);
            Ok((
                r.get::<_, i64>(0)?,
                serde_json::json!({
                    "kind": r.get::<_, String>(1)?,
                    "ts": r.get::<_, String>(2)?,
                    "payload": payload_v
                }),
            ))
        })? {
            let (t, v) = r?;
            out.insert(t, v);
        }
    }
    Ok(out)
}

/// All task ids in `root_task_id`'s transitive dependency subtree, inclusive
/// of the root itself.
pub fn subtree_ids(conn: &Connection, root_task_id: i64) -> Result<HashSet<i64>> {
    let mut s = conn.prepare(
        "WITH RECURSIVE sub(id) AS (
            SELECT ?1
            UNION
            SELECT d.depends_on_task_id FROM dep d JOIN sub ON d.task_id = sub.id
         ) SELECT id FROM sub",
    )?;
    let ids: HashSet<i64> = s
        .query_map([root_task_id], |r| r.get::<_, i64>(0))?
        .collect::<Result<_, _>>()?;
    Ok(ids)
}
