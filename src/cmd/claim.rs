//! The `assigned` → `running` edge.
//!
#![doc = include_str!("../../docs/modules/claim.md")]

use crate::outcome::Outcome;
use crate::{db, id, lease};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimArgs {
    pub task: String,
    #[arg(long = "as")]
    pub agent: String,
    /// How long this hold may sit quiet before it reads as stale. A unit
    /// suffix is required: 30s, 45m, 8h, 2d. Defaults to 8h.
    #[arg(long = "for")]
    #[serde(default)]
    pub ttl: Option<String>,
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
}

impl crate::route::Served for ClaimArgs {
    const VERB: &'static str = "claim";
    type Out = Claimed;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct Claimed {
    display_id: String,
    agent_id: String,
    state: String,
    expires_at: Option<String>,
}
impl Outcome for Claimed {
    fn human(&self) -> String {
        match &self.expires_at {
            Some(e) => format!(
                "{} claimed by {} (stale if quiet past {})",
                self.display_id, self.agent_id, e
            ),
            None => format!("{} claimed by {}", self.display_id, self.agent_id),
        }
    }
}

pub fn exec(db_path: &std::path::Path, a: ClaimArgs) -> Result<Claimed> {
    // Parsed before anything else moves: a lease length the operator got wrong
    // must fail with nothing claimed, not leave a ticket held under a length
    // nobody meant.
    let ttl = lease::parse_ttl(a.ttl.as_deref().unwrap_or(lease::DEFAULT_TTL))?;
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    db::with_tx(&mut conn, |tx| {
        // Latest open assignment must be (a) for this agent (b) un-claimed.
        let Some(open) = db::current_assignment(tx, task_id)? else {
            return Err(db::no_open_assignment(
                tx,
                task_id,
                &resolved.display_id,
                Some(&a.agent),
                format!("{} has no assignment", a.task),
            )?);
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
            "UPDATE task SET state = ?1 WHERE id = ?2 AND state = ?3",
            rusqlite::params![db::State::Running, task_id, db::State::Assigned],
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
        // The lease starts with the hold. Stamped before the event below, so
        // the renewal that rides on `insert_event` has a lease to renew.
        lease::take(tx, aid, ttl)?;
        // `from` is a literal: the guarded UPDATE above matched `assigned` only.
        db::insert_state_change(
            tx,
            task_id,
            Some(db::State::Assigned.as_str()),
            db::State::Running.as_str(),
            Some(&a.agent),
            &[],
        )?;
        Ok(())
    })?;
    let expires_at = lease::hold_of(&conn, task_id)?.and_then(|h| h.expires_at);
    Ok(Claimed {
        display_id: resolved.display_id,
        agent_id: a.agent,
        state: "running".to_string(),
        expires_at,
    })
}
