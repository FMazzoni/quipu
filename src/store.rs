//! Canonical queries over the qp schema.
//!
//! Layering (see `$QUIPU_VAULT/plans/2026-07-18-185716-audit-remediation.md`):
//!   `db.rs`    — connection, transactions, migrations, guarded-transition helpers
//!   `store.rs` — canonical read queries + the row types they return
//!   `cmd/*.rs` — argument parsing and rendering only, no SQL
//!
//! Why this module exists: the same queries were hand-written across many
//! command files in subtly divergent forms — the "latest agent" lookup existed
//! in 3 shapes across 11 sites, the unresolved-dep predicate in 9, the
//! event-tail SELECT in 3 column shapes across 6. Divergence is the risk, not
//! verbosity: adding a terminal state means updating every copy correctly, and
//! missing one is a silent logic bug.
//!
//! Scope discipline (deliberate, from the QP-68 research):
//!   - Read queries and their row types belong here.
//!   - Guarded-transition UPDATEs do NOT. They are not duplicated with each
//!     other — each has a distinct WHERE/SET — so moving them would relocate
//!     the highest-stakes code in the project for taxonomic tidiness alone.
//!   - Rendering helpers do NOT. `wrap_text`, `md_esc`, `html_esc`, `slugify`
//!     do no database work.

#![allow(dead_code)] // populated incrementally; some helpers land before their callers

// --- QP-68: event-tail query family -----------------------------------
//
// `timeline.rs`, `watch.rs`, and `decisions.rs` each hand-rolled the same
// `event LEFT JOIN task` SELECT with gratuitously different `?N` numbering
// and repeated the same payload-parse-or-Null idiom. `EventFilter` is the
// smallest shape that covers all three call sites:
//   - `since_id`: timeline/watch always pass `Some(since)` (default 0);
//     decisions passes `None` (no lower bound — it wants the full history).
//   - `task_id`: timeline/watch resolve `--task` to an id; decisions has no
//     task filter.
//   - `kinds`: an IN-list. timeline/watch pass whatever `--kind` flags the
//     caller gave (possibly empty = no filter); decisions passes the
//     single-element slice `["decision"]`, which is just IN-list-of-one.
//   - `auto_only`: only decisions sets this; it adds the
//     `json_extract(payload, '$.auto') = 1` predicate.
// All three order `ORDER BY e.id ASC` unconditionally, so there is no
// direction field — a speculative `ascending` flag would be unused by every
// current caller.

/// One row of the `event LEFT JOIN task` tail, payload already parsed.
pub struct EventRow {
    pub id: i64,
    pub task: Option<String>, // display_id
    pub ts: String,
    pub kind: String,
    pub agent: Option<String>,
    pub payload: serde_json::Value, // parsed; malformed/absent payload -> Null
}

/// Filter for [`events`]. All fields are conjunctive (`AND`); `None`/empty
/// means "no constraint on this dimension".
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

/// The event tail: `event LEFT JOIN task`, filtered by `f`, ordered
/// `e.id ASC`. Bound parameters throughout — no string-interpolated values.
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
