//! `qp block` — create a blocker task and link it as an unresolved dep.
//!
//! Convenience wrapper. Equivalent to:
//!
//! ```text
//! qp add "<new>" --tag kind:blocker
//! qp depends <task> --on <new-id> --as <agent>
//! ```
//!     qp abandon <task> --as <agent>
//!
//! collapsed into one transaction so partial failures can't leave a dangling task.

use crate::outcome::{emit, Outcome};
use crate::{db, id};
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
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct Blocked {
    display_id: String,
    blocker_id: String,
    blocker_title: String,
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
        tx.execute(
            "INSERT OR IGNORE INTO tag(task_id, name) VALUES (?, 'kind:blocker')",
            [blocker_id],
        )?;

        // (2) Insert dep edge orig → blocker. No cycle possible (blocker is brand-new).
        tx.execute(
            "INSERT INTO dep(task_id, depends_on_task_id) VALUES (?,?)",
            rusqlite::params![task_id, blocker_id],
        )?;

        // (3) Guarded UPDATE: demote orig to pending. Folds ownership into WHERE via EXISTS —
        // this remains the single source of truth for the mutation, per the guarded-transition
        // contract. If it fails, the diagnostic reads below are for error reporting only (not
        // control flow) so the caller can tell wrong-agent (NotOwner) from wrong-state (Conflict).
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
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            Some(&a.agent),
            Some(&serde_json::json!({"to": "pending", "via": "block"})),
        )?;
        Ok(blocker_display)
    })?;
    emit(
        a.json,
        &Blocked {
            display_id: resolved.display_id,
            blocker_id: blocker_display,
            blocker_title: a.new,
            state: "pending".to_string(),
        },
    )
}
