//! The orchestrator-side release edge: `assigned`/`running` → `pending`.
//!
#![doc = include_str!("../../docs/modules/reclaim.md")]

use crate::effects::{self, Effect};
use crate::outcome::Outcome;
use crate::{db, id, lease};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReclaimArgs {
    pub task: String,
    #[arg(long)]
    #[serde(default)]
    pub reason: Option<String>,
    /// Break a hold that is still live. Without it only a stale hold — one
    /// whose holder has been quiet past its lease — can be released.
    #[arg(long)]
    #[serde(default)]
    pub force: bool,
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
}

impl crate::route::Served for ReclaimArgs {
    const VERB: &'static str = "reclaim";
    type Out = Reclaimed;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct Reclaimed {
    display_id: String,
    state: String,
    reason: Option<String>,
    forced: bool,
    /// Tasks this transaction moved that the caller did not name.
    #[serde(rename = "_effects", default, skip_serializing_if = "Vec::is_empty")]
    effects: Vec<Effect>,
}
impl Outcome for Reclaimed {
    fn human(&self) -> String {
        format!("{} reclaimed", self.display_id)
    }
}

pub fn exec(db_path: &std::path::Path, a: ReclaimArgs) -> Result<Reclaimed> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    let (resulting, effects) = db::with_tx(&mut conn, |tx| {
        // Origin read first, for the event payload only — the guard names a set,
        // so the log cannot tell `assigned` from `running` otherwise. The
        // `UPDATE` stays conditional and `changes() == 1` still decides.
        let from = db::state_of(tx, task_id)?;
        // Staleness is part of the guard, not a check before it. One
        // conditional `UPDATE` decides both that the task was in flight and
        // that its hold had expired, so two machines racing to reclaim the same
        // dead hold produce exactly one winner — and a hold renewed between the
        // read and the write survives, because there is no read to race with.
        let sql = format!(
            "UPDATE task SET state = ?1 WHERE id = ?2 AND state IN ('assigned','running')
               AND (?3 = 1 OR NOT EXISTS (
                     SELECT 1 FROM assignment a
                      WHERE a.task_id = task.id AND a.completed_at IS NULL
                        AND {live}))",
            live = lease::LIVE_HOLD
        );
        let n = tx.execute(
            &sql,
            rusqlite::params![db::State::Pending, task_id, i64::from(a.force)],
        )?;
        if n != 1 {
            // Read for the message only; the `UPDATE` above already decided.
            let state = db::state_of(tx, task_id)?;
            if matches!(state.as_deref(), Some("assigned") | Some("running")) {
                let held = lease::hold_of(tx, task_id)?;
                let who = held
                    .as_ref()
                    .map(|h| match &h.host {
                        Some(host) => format!("`{}` on `{host}`", h.agent_id),
                        None => format!("`{}`", h.agent_id),
                    })
                    .unwrap_or_else(|| "someone".to_string());
                let until = held
                    .as_ref()
                    .and_then(|h| h.expires_at.clone())
                    .map(|e| format!(" until {e}"))
                    .unwrap_or_else(|| " indefinitely (the hold states no expiry)".to_string());
                return Err(db::conflict(
                    "hold_is_live",
                    format!(
                        "{} is held by {who}{until}; the holder has not gone quiet long \
                         enough to be treated as gone. Reclaiming a live hold takes the \
                         force flag.",
                        a.task
                    ),
                    Some(resolved.display_id.clone()),
                ));
            }
            return Err(db::conflict(
                "not_assigned_or_running",
                format!("{} not assigned/running", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        // The release and the re-promotion are two moves, and get two events —
        // see the note in `abandon`, which this mirrors.
        db::insert_state_change(
            tx,
            task_id,
            from.as_deref(),
            db::State::Pending.as_str(),
            None,
            &[
                ("via", serde_json::json!("reclaim")),
                ("reason", serde_json::json!(a.reason)),
            ],
        )?;
        let promoted = db::refresh_ready_logged(tx, None, "reclaim")?;
        let named_returned = promoted.contains(&task_id);
        let effects = effects::collateral(
            tx,
            &[task_id],
            "reclaim",
            &db::Movements::promoted(promoted),
        )?;
        let resulting = if named_returned {
            db::State::Ready.to_string()
        } else {
            db::State::Pending.to_string()
        };

        let n2 = tx.execute(
            "UPDATE assignment SET completed_at = ?, outcome = 'reclaimed'
              WHERE task_id = ? AND completed_at IS NULL",
            rusqlite::params![crate::time::now_rfc3339(), task_id],
        )?;
        if n2 == 0 {
            return Err(db::conflict(
                "no_open_assignment",
                format!("{} has no open assignment rows to close", a.task),
                Some(resolved.display_id.clone()),
            ));
        }
        Ok((resulting, effects))
    })?;
    Ok(Reclaimed {
        display_id: resolved.display_id,
        state: resulting,
        reason: a.reason,
        forced: a.force,
        effects,
    })
}
