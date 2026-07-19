//! The `running` → `done` edge.
//!
//! Records decisions and artifacts as events on the way through.

use crate::outcome::{emit, Outcome};
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct CompleteArgs {
    pub task: String,
    #[arg(long = "as")]
    pub agent: String,
    #[arg(long, value_name = "TEXT")]
    pub decision: Vec<String>,
    #[arg(long = "artifact", value_name = "PATH")]
    pub artifact: Vec<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct Completed {
    display_id: String,
    state: String,
    decisions: Vec<String>,
    artifacts: Vec<String>,
}
impl Outcome for Completed {
    fn human(&self) -> String {
        format!("{} done", self.display_id)
    }
}

pub fn run(db_path: &std::path::Path, a: CompleteArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    db::with_tx(&mut conn, |tx| {
        let Some(open) = db::current_assignment(tx, task_id)? else {
            return Err(db::conflict(
                "no_open_assignment",
                format!("{} not assigned", a.task),
                Some(resolved.display_id.clone()),
            ));
        };
        let aid = open.id;
        if open.claimed_at.is_none() {
            return Err(db::conflict(
                "not_claimed",
                format!("{} not claimed", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        if open.agent_id != a.agent {
            return Err(db::not_owner(
                format!("{} not yours", a.task),
                Some(resolved.display_id.clone()),
                Some(open.agent_id.clone()),
            ));
        }
        let n = tx.execute(
            "UPDATE task SET state = ?1 WHERE id = ?2 AND state = ?3",
            rusqlite::params![db::State::Done, task_id, db::State::Running],
        )?;
        if n != 1 {
            return Err(db::conflict(
                "not_running",
                format!("{} not in running state", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        tx.execute(
            "UPDATE assignment SET completed_at = ?, outcome = 'success' WHERE id = ?",
            rusqlite::params![crate::time::now_rfc3339(), aid],
        )?;
        for d in &a.decision {
            db::insert_event(
                tx,
                Some(task_id),
                "decision",
                Some(&a.agent),
                Some(&serde_json::json!({"text": d})),
            )?;
        }
        for p in &a.artifact {
            db::insert_event(
                tx,
                Some(task_id),
                "artifact",
                Some(&a.agent),
                Some(&serde_json::json!({"path": p})),
            )?;
        }
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            Some(&a.agent),
            Some(&serde_json::json!({"to": "done"})),
        )?;
        db::refresh_ready(tx)?;
        Ok(())
    })?;
    emit(
        a.json,
        &Completed {
            display_id: resolved.display_id,
            state: "done".to_string(),
            decisions: a.decision,
            artifacts: a.artifact,
        },
    )
}
