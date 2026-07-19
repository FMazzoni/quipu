//! The `ready` → `assigned` edge.
//!
//! Orchestrator-only. Agents take work with `qp claim`, never this.
//!
//! # Why the `stale_open_assignment` guard is defensive, not dead
//!
//! The INSERT below is conditional on no open (`completed_at IS NULL`)
//! assignment existing for the task, and reports `stale_open_assignment` when
//! that condition fails. No CLI sequence is known to reach it: the codebase
//! maintains an invariant that an open assignment row exists only while the
//! task is `assigned` or `running`, and the `WHERE state = 'ready'` guard above
//! trips first for every task that satisfies it. Two racing `qp assign`
//! processes therefore make the loser report `not_ready`, never
//! `stale_open_assignment`.
//!
//! That invariant is *emergent, not enforced*. It holds only because every
//! command that moves a task out of `assigned`/`running` also closes the
//! assignment in the same transaction — `abandon`, `reclaim`, `block`,
//! `cancel`, `complete` — and because this module is the only `INSERT INTO
//! assignment` in the tree. Nothing in `schema.sql` enforces it: there is no
//! partial unique index on `assignment(task_id) WHERE completed_at IS NULL`.
//! A new command that demotes a task without closing its assignment, or a
//! reordering inside any of those five, silently makes this branch live.
//!
//! So the guard stays. Deleting it would trade a cheap conditional INSERT for
//! a silent second open assignment row, which breaks `db::current_assignment`'s
//! "at most one" premise and makes latest-open and latest-by-id disagree — a
//! corruption that surfaces far from its cause. `tests/cli.rs`'s
//! `open_assignment_implies_assigned_or_running` pins the premise: if it ever
//! fails, this branch is no longer defensive and the failure is the warning.

use crate::outcome::{emit, Outcome};
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct AssignArgs {
    pub task: String,
    #[arg(long = "to")]
    pub to: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct Assigned {
    display_id: String,
    agent_id: String,
    state: String,
}
impl Outcome for Assigned {
    fn human(&self) -> String {
        format!("{} assigned to {}", self.display_id, self.agent_id)
    }
}

pub fn run(db_path: &std::path::Path, a: AssignArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    db::with_tx(&mut conn, |tx| {
        let n = tx.execute(
            "UPDATE task SET state = ?1 WHERE id = ?2 AND state = ?3",
            rusqlite::params![db::State::Assigned, task_id, db::State::Ready],
        )?;
        if n != 1 {
            return Err(db::conflict(
                "not_ready",
                format!("{} not ready for assignment", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        let n = tx.execute(
            "INSERT INTO assignment(task_id, agent_id)
             SELECT ?1, ?2 WHERE NOT EXISTS (
               SELECT 1 FROM assignment WHERE task_id = ?1 AND completed_at IS NULL)",
            rusqlite::params![task_id, a.to],
        )?;
        if n != 1 {
            // Defensive: unreachable via any known CLI sequence, because a `ready`
            // task should never carry an open assignment. See the module header for
            // why that premise is not enforced and the guard therefore stays.
            return Err(db::conflict(
                "stale_open_assignment",
                format!("{} has a stale open assignment", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            Some(&a.to),
            Some(&serde_json::json!({"to": "assigned", "agent_id": a.to})),
        )?;
        Ok(())
    })?;
    emit(
        a.json,
        &Assigned {
            display_id: resolved.display_id,
            agent_id: a.to,
            state: "assigned".to_string(),
        },
    )
}
