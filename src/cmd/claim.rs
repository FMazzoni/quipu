use crate::outcome::{emit, Outcome};
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct ClaimArgs {
    pub task: String,
    #[arg(long = "as")]
    pub agent: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct Claimed {
    display_id: String,
    agent_id: String,
    state: String,
}
impl Outcome for Claimed {
    fn human(&self) -> String {
        format!("{} claimed by {}", self.display_id, self.agent_id)
    }
}

pub fn run(db_path: &std::path::Path, a: ClaimArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    db::with_tx(&mut conn, |tx| {
        // Latest open assignment must be (a) for this agent (b) un-claimed.
        let Some(open) = db::current_assignment(tx, task_id)? else {
            return Err(db::conflict(
                "no_open_assignment",
                format!("{} has no assignment", a.task),
                Some(resolved.display_id.clone()),
            ));
        };
        let aid = open.id;
        if open.claimed_at.is_some() {
            return Err(db::conflict(
                "already_claimed",
                format!("{} already claimed", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        if open.agent_id != a.agent {
            return Err(db::not_owner(
                format!(
                    "{} assigned to `{}`, not `{}`",
                    a.task, open.agent_id, a.agent
                ),
                Some(resolved.display_id.clone()),
                Some(open.agent_id.clone()),
            ));
        }
        // Guarded transition: task must currently be `assigned`.
        let n = tx.execute(
            "UPDATE task SET state = 'running' WHERE id = ? AND state = 'assigned'",
            [task_id],
        )?;
        if n != 1 {
            return Err(db::conflict(
                "state_changed_under_us",
                format!(
                    "{} changed state before the claim landed; re-check and retry",
                    a.task
                ),
                Some(resolved.display_id.clone()),
            ));
        }
        tx.execute(
            "UPDATE assignment SET claimed_at = ? WHERE id = ?",
            rusqlite::params![crate::time::now_rfc3339(), aid],
        )?;
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            Some(&a.agent),
            Some(&serde_json::json!({"to": "running"})),
        )?;
        Ok(())
    })?;
    emit(
        a.json,
        &Claimed {
            display_id: resolved.display_id,
            agent_id: a.agent,
            state: "running".to_string(),
        },
    )
}
