//! `qp block` — convenience wrapper that creates a new blocker task and links it
//! as an unresolved dep on the original. Equivalent to:
//!
//!     qp add "<new>" --tag kind:blocker
//!     qp depends <task> --on <new-id> --as <agent>
//!     qp abandon <task> --as <agent>
//!
//! collapsed into one transaction so partial failures can't leave a dangling task.

use crate::{db, id};
use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct BlockArgs {
    pub task: String,
    #[arg(long = "as")]
    pub agent: String,
    #[arg(long = "new", value_name = "TITLE")]
    pub new: String,
}

pub fn run(db_path: &std::path::Path, a: BlockArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;

    let blocker_display = db::with_tx(&mut conn, |tx| {
        // (1) Create the blocker task. State = ready (no deps of its own).
        let prefix = db::display_prefix(tx)?;
        tx.execute(
            "INSERT INTO task(display_id, title, state) VALUES ('', ?, 'ready')",
            rusqlite::params![a.new],
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

        // (3) Guarded UPDATE: demote orig to pending. Folds ownership into WHERE via EXISTS.
        let n = tx.execute(
            "UPDATE task SET state = 'pending'
              WHERE id = ?1 AND state IN ('assigned','running')
                AND EXISTS (SELECT 1 FROM assignment
                             WHERE task_id = ?1 AND agent_id = ?2 AND completed_at IS NULL)",
            rusqlite::params![task_id, a.agent],
        )?;
        if n != 1 {
            return Err(db::constraint(format!(
                "{} not yours or not blockable",
                a.task
            )));
        }

        // (4) Close the in-flight assignment by specific id (mirrors abandon.rs pattern).
        let Some(open) = db::current_assignment(tx, task_id)? else {
            return Err(db::constraint(format!(
                "no open assignment to close for {}",
                a.task
            )));
        };
        let aid = open.id;
        let n = tx.execute(
            "UPDATE assignment SET completed_at = ?, outcome = 'abandoned' WHERE id = ?",
            rusqlite::params![crate::time::now_rfc3339(), aid],
        )?;
        if n != 1 {
            return Err(db::constraint(format!(
                "no open assignment to close for {}",
                a.task
            )));
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
    println!("{} blocked by {}", resolved.display_id, blocker_display);
    Ok(())
}
