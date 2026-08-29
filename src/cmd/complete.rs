//! The `running` → `done` edge.
//!
#![doc = include_str!("../../docs/modules/complete.md")]

use crate::effects::{self, Effect};
use crate::outcome::Outcome;
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteArgs {
    pub task: String,
    #[arg(long = "as")]
    pub agent: String,
    #[arg(long, value_name = "TEXT")]
    #[serde(default)]
    pub decision: Vec<String>,
    #[arg(long = "artifact", value_name = "PATH")]
    #[serde(default)]
    pub artifact: Vec<String>,
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
}

impl crate::route::Served for CompleteArgs {
    const VERB: &'static str = "complete";
    type Out = Completed;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct Completed {
    display_id: String,
    state: String,
    decisions: Vec<String>,
    artifacts: Vec<String>,
    /// Tasks this transaction moved that the caller did not name.
    #[serde(rename = "_effects", default, skip_serializing_if = "Vec::is_empty")]
    effects: Vec<Effect>,
}
impl Outcome for Completed {
    fn human(&self) -> String {
        format!("{} done", self.display_id)
    }
}

pub fn exec(db_path: &std::path::Path, a: CompleteArgs) -> Result<Completed> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    let effects = db::with_tx(&mut conn, |tx| {
        let Some(open) = db::current_assignment(tx, task_id)? else {
            return Err(db::no_open_assignment(
                tx,
                task_id,
                &resolved.display_id,
                Some(&a.agent),
                format!("{} not assigned", a.task),
            )?);
        };
        let aid = open.id;
        // Ownership before claimed-ness: when both are wrong, "this is not
        // yours" is the answer the caller can act on, and it is the only one
        // that can name a holder whose lease was broken while it was away.
        if open.agent_id != a.agent {
            // A holder that was dispossessed while it was away is told so by
            // name, rather than being handed the generic "not yours".
            if let Some(e) = crate::lease::dispossessed(tx, task_id, &a.agent)? {
                return Err(e);
            }
            return Err(db::not_owner(
                format!("{} not yours", a.task),
                Some(resolved.display_id.clone()),
                Some(open.agent_id.clone()),
            ));
        }
        if open.claimed_at.is_none() {
            return Err(db::conflict(
                "not_claimed",
                format!("{} not claimed", a.task),
                Some(resolved.display_id.clone()),
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
        // `from` is the state the guarded UPDATE above already required.
        db::insert_state_change(
            tx,
            task_id,
            Some(db::State::Running.as_str()),
            db::State::Done.as_str(),
            Some(&a.agent),
            &[],
        )?;
        // Logged: finishing a blocker is the single most common way work gets
        // unblocked in this system, and the bare sweep left that promotion —
        // the entire point of the dependency graph — with no event at all.
        let promoted = db::refresh_ready_logged(tx, Some(&a.agent), "complete")?;
        effects::collateral(
            tx,
            &[task_id],
            "complete",
            &db::Movements::promoted(promoted),
        )
    })?;
    Ok(Completed {
        display_id: resolved.display_id,
        state: "done".to_string(),
        decisions: a.decision,
        artifacts: a.artifact,
        effects,
    })
}
