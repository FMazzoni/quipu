use crate::outcome::{emit, Outcome};
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct CancelArgs {
    pub task: String,
    #[arg(long)]
    pub reason: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct Cancelled {
    display_id: String,
    state: String,
    reason: Option<String>,
}
impl Outcome for Cancelled {
    fn human(&self) -> String {
        format!("{} cancelled", self.display_id)
    }
}

pub fn run(db_path: &std::path::Path, a: CancelArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    db::with_tx(&mut conn, |tx| {
        let n = tx.execute(
            "UPDATE task SET state = 'cancelled'
              WHERE id = ? AND state NOT IN ('done','cancelled')",
            [task_id],
        )?;
        if n != 1 {
            return Err(db::conflict(
                "already_terminal",
                format!("{} already terminal", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        tx.execute(
            "UPDATE assignment SET outcome = 'cancelled', completed_at = COALESCE(completed_at, ?)
              WHERE task_id = ? AND completed_at IS NULL",
            rusqlite::params![crate::time::now_rfc3339(), task_id],
        )?;
        db::insert_event(
            tx,
            Some(task_id),
            "state_change",
            None,
            Some(&serde_json::json!({"to": "cancelled", "reason": a.reason})),
        )?;
        db::refresh_ready(tx)?;
        Ok(())
    })?;
    emit(
        a.json,
        &Cancelled {
            display_id: resolved.display_id,
            state: "cancelled".to_string(),
            reason: a.reason,
        },
    )
}
