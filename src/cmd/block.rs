//! `qp block` — create a blocker task and link it as an unresolved dep.
//!
#![doc = include_str!("../../docs/modules/block.md")]

use crate::outcome::{emit, Outcome};
use crate::{claimref, db, id};
use anyhow::Result;
use clap::Args;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct BlockArgs {
    pub task: String,
    #[arg(long = "as")]
    pub agent: String,
    #[arg(long = "new", value_name = "TITLE")]
    pub new: String,
    /// Tag(s) for the new blocker task. Repeatable; replaces the default when given.
    #[arg(long, value_name = "NAME")]
    pub tag: Vec<String>,
    #[arg(long)]
    pub json: bool,
}

/// The skill-layer convention `qp block` defaults to. See the module header.
const DEFAULT_BLOCKER_TAG: &str = "kind:blocker";

#[derive(Serialize)]
struct Blocked {
    display_id: String,
    blocker_id: String,
    blocker_title: String,
    blocker_tags: Vec<String>,
    state: String,
}
impl Outcome for Blocked {
    fn human(&self) -> String {
        format!("{} blocked by {}", self.display_id, self.blocker_id)
    }
}

pub fn run(db_path: &std::path::Path, a: BlockArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    // `block` releases the claim like the other four release edges do: it is
    // `assigned`/`running` -> `pending`, so leaving the ref behind would strand
    // the ticket as unclaimable with nobody holding it.
    let release = claimref::release_for(db_path, &conn, task_id, &a.agent)?;

    // Empty means "caller expressed no taxonomy", so fall back to the convention.
    // Dedup+sort so the emitted `blocker_tags` matches what actually landed in the
    // table (INSERT OR IGNORE would otherwise let a repeated --tag show up twice).
    let mut tags: Vec<String> = if a.tag.is_empty() {
        vec![DEFAULT_BLOCKER_TAG.to_string()]
    } else {
        a.tag.clone()
    };
    tags.sort();
    tags.dedup();
    for t in &tags {
        if t.trim().is_empty() {
            return Err(db::invalid_input("tag must not be empty"));
        }
    }

    let blocker_display = db::with_tx(&mut conn, |tx| {
        // (1) Create the blocker task. State = ready (no deps of its own).
        let prefix = db::display_prefix(tx)?;
        tx.execute(
            "INSERT INTO task(display_id, title, state) VALUES ('', ?1, ?2)",
            rusqlite::params![a.new, db::State::Ready],
        )?;
        let blocker_id = tx.last_insert_rowid();
        let blocker_display = id::encode(blocker_id, &prefix);
        tx.execute(
            "UPDATE task SET display_id = ? WHERE id = ?",
            rusqlite::params![blocker_display, blocker_id],
        )?;
        // The blocker's own creation event. Until this existed, every event
        // this command wrote was filed against the *blocked* task, so the new
        // task had no `state_change` of its own and a replay of the log could
        // never bring it into existence — it appeared only as the far end of an
        // edge pointing at nothing.
        db::insert_state_change(
            tx,
            blocker_id,
            None,
            db::State::Ready.as_str(),
            Some(&a.agent),
            &[("title", serde_json::json!(a.new))],
        )?;
        for t in &tags {
            tx.execute(
                "INSERT OR IGNORE INTO tag(task_id, name) VALUES (?,?)",
                rusqlite::params![blocker_id, t],
            )?;
            if tx.changes() == 1 {
                db::insert_event(
                    tx,
                    Some(blocker_id),
                    "tag_added",
                    Some(&a.agent),
                    Some(&serde_json::json!({"name": t})),
                )?;
            }
        }

        // (2) Insert dep edge orig → blocker. No cycle possible (blocker is
        // brand-new), so the edge goes in directly rather than through
        // `db::link_dep` — its cycle check and its guarded `ready` demote are
        // both dead weight here, since step (3) below demotes from
        // `assigned`/`running` instead.
        //
        // The propagation half is NOT dead weight. If the blocked task is a
        // container, this blocker freezes everything inside it, and skipping
        // that left `qp block` as the one blocking verb whose freeze was
        // reported by `qp show` but never applied — `qp list --state ready`
        // would keep handing the frozen slices out. `qp block` is how an agent
        // raises a blocker mid-wave, so that was the main path the feature
        // exists to serve.
        tx.execute(
            "INSERT INTO dep(task_id, depends_on_task_id, mode) VALUES (?,?,?)",
            rusqlite::params![task_id, blocker_id, db::MODE_BLOCKS],
        )?;
        // The edge still goes in directly, but it no longer goes in silently:
        // `link_dep` is what normally writes `dep_added`, and skipping it left
        // this edge invisible to anything replaying the graph from the log.
        db::insert_event(
            tx,
            Some(task_id),
            "dep_added",
            Some(&a.agent),
            Some(&serde_json::json!({"on": blocker_display, "mode": db::MODE_BLOCKS})),
        )?;
        db::demote_frozen(tx, Some(&a.agent), "block")?;

        // (3) Guarded UPDATE: demote orig to pending. The origin is read first,
        // for the event payload only — the guard admits both `assigned` and
        // `running`, and the `UPDATE` stays conditional either way.
        let from = db::state_of(tx, task_id)?;
        let n = tx.execute(
            "UPDATE task SET state = ?1
              WHERE id = ?2 AND state IN ('assigned','running')
                AND EXISTS (SELECT 1 FROM assignment
                             WHERE task_id = ?2 AND agent_id = ?3 AND completed_at IS NULL)",
            rusqlite::params![db::State::Pending, task_id, a.agent],
        )?;
        if n != 1 {
            let cur_state: Option<String> = tx
                .query_row("SELECT state FROM task WHERE id = ?", [task_id], |r| {
                    r.get(0)
                })
                .ok();
            let state_ok = matches!(cur_state.as_deref(), Some("assigned") | Some("running"));
            if !state_ok {
                // A task that left `running` under this agent's feet is usually
                // one whose hold was reclaimed. Say so by name before falling
                // back to the state-shaped message.
                if let Some(e) = crate::lease::dispossessed(tx, task_id, &a.agent)? {
                    return Err(e);
                }
                return Err(db::conflict(
                    "not_blockable",
                    format!(
                        "{} is not assigned/running (state={})",
                        a.task,
                        cur_state.as_deref().unwrap_or("unknown")
                    ),
                    Some(resolved.display_id.clone()),
                ));
            }
            // A holder that was dispossessed while it was away is told so by
            // name, rather than being handed the generic "not yours".
            if let Some(e) = crate::lease::dispossessed(tx, task_id, &a.agent)? {
                return Err(e);
            }
            return Err(db::not_owner(
                format!("{} is not yours to block", a.task),
                Some(resolved.display_id.clone()),
                None,
            ));
        }

        // (4) Close the in-flight assignment by specific id (mirrors abandon.rs pattern).
        // Unreachable in practice given (3) just confirmed an open assignment for this agent —
        // defensive only.
        let Some(open) = db::current_assignment(tx, task_id)? else {
            return Err(db::conflict(
                "no_open_assignment",
                format!("no open assignment to close for {}", a.task),
                Some(resolved.display_id.clone()),
            ));
        };
        let aid = open.id;
        let n = tx.execute(
            "UPDATE assignment SET completed_at = ?, outcome = 'abandoned' WHERE id = ?",
            rusqlite::params![crate::time::now_rfc3339(), aid],
        )?;
        if n != 1 {
            return Err(db::conflict(
                "no_open_assignment",
                format!("no open assignment to close for {}", a.task),
                Some(resolved.display_id.clone()),
            ));
        }

        // (5) One `blocker` event with structured payload (skill-readable).
        db::insert_event(
            tx,
            Some(task_id),
            "blocker",
            Some(&a.agent),
            Some(&serde_json::json!({
                "blocker_id": blocker_display, "title": a.new
            })),
        )?;
        // Plus a state_change event so timeline/watch reflect the demotion.
        db::insert_state_change(
            tx,
            task_id,
            from.as_deref(),
            db::State::Pending.as_str(),
            Some(&a.agent),
            &[("via", serde_json::json!("block"))],
        )?;
        Ok(blocker_display)
    })?;
    if let Some(r) = &release {
        r.finish(&conn);
    }
    emit(
        a.json,
        &Blocked {
            display_id: resolved.display_id,
            blocker_id: blocker_display,
            blocker_title: a.new,
            blocker_tags: tags,
            state: "pending".to_string(),
        },
    )
}
