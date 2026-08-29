//! The `ready` → `assigned` edge, where the taker also chooses which task.
//!
#![doc = include_str!("../../docs/modules/take.md")]

use crate::db;
use crate::outcome::{emit, Outcome};
use anyhow::Result;
use clap::Args;
use rusqlite::{OptionalExtension, ToSql};
use serde::Serialize;

#[derive(Args, Debug)]
pub struct TakeArgs {
    #[arg(long = "as")]
    pub agent: String,
    /// Glob pattern (e.g. kind:slice); repeatable, all must match
    #[arg(long)]
    pub tag: Vec<String>,
    #[arg(long)]
    pub json: bool,
}

/// The task a `take` won, absent when the queue had nothing for this caller.
#[derive(Serialize)]
struct Taken {
    taken: Option<TakenTask>,
    agent_id: String,
}

#[derive(Serialize)]
struct TakenTask {
    display_id: String,
    title: String,
    state: String,
}

impl Outcome for Taken {
    fn human(&self) -> String {
        match &self.taken {
            // Same wording as `assign`, because it is the same edge.
            Some(t) => format!("{} assigned to {}", t.display_id, self.agent_id),
            None => "nothing to take".to_string(),
        }
    }
}

/// The one statement that both chooses and takes.
///
/// `{tags}` is replaced by zero or more tag-glob `EXISTS` clauses, each taking
/// one bound parameter, mirroring `store::tasks`.
const TAKE_SQL: &str = "{FROZEN} UPDATE task
        SET state = 'assigned'
      WHERE id = (
              SELECT t.id FROM task t
               WHERE t.state = 'ready'
                 AND t.id NOT IN (SELECT id FROM frozen)
                 AND NOT EXISTS (
                       SELECT 1 FROM dep d
                         JOIN task t2 ON t2.id = d.depends_on_task_id
                        WHERE d.task_id = t.id
                          AND t2.state NOT IN ('done','cancelled'))
                 {tags}
               ORDER BY t.id ASC
               LIMIT 1)
        AND state = 'ready'
  RETURNING id, display_id, title";

pub fn run(db_path: &std::path::Path, a: TakeArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let tags = a
        .tag
        .iter()
        .map(|_| {
            " AND EXISTS (SELECT 1 FROM tag WHERE tag.task_id = t.id AND tag.name GLOB ?)"
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("");
    let sql = TAKE_SQL
        .replace("{FROZEN}", db::FROZEN)
        .replace("{tags}", &tags);

    let taken = db::with_tx(&mut conn, |tx| {
        let params: Vec<&dyn ToSql> = a.tag.iter().map(|t| t as &dyn ToSql).collect();
        let row: Option<(i64, String, String)> = {
            let mut stmt = tx.prepare(&sql)?;
            stmt.query_row(params.as_slice(), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?
        };
        // No candidate matched. Not an error: see the module page.
        let Some((task_id, display_id, title)) = row else {
            return Ok(None);
        };
        if tx.changes() != 1 {
            return Err(db::conflict(
                "take_matched_multiple",
                "take matched more than one task; refusing",
                Some(display_id.clone()),
            ));
        }
        let n = tx.execute(
            "INSERT INTO assignment(task_id, agent_id)
             SELECT ?1, ?2 WHERE NOT EXISTS (
               SELECT 1 FROM assignment WHERE task_id = ?1 AND completed_at IS NULL)",
            rusqlite::params![task_id, a.agent],
        )?;
        if n != 1 {
            // Same defensive guard as `assign`; see `docs/modules/assign.md`.
            return Err(db::conflict(
                "stale_open_assignment",
                format!("{display_id} has a stale open assignment"),
                Some(display_id.clone()),
            ));
        }
        // `from` is a literal: the guarded UPDATE above matched `ready` only.
        db::insert_state_change(
            tx,
            task_id,
            Some(db::State::Ready.as_str()),
            db::State::Assigned.as_str(),
            Some(&a.agent),
            &[("agent_id", serde_json::json!(a.agent))],
        )?;
        Ok(Some(TakenTask {
            display_id,
            title,
            state: db::State::Assigned.as_str().to_string(),
        }))
    })?;

    emit(
        a.json,
        &Taken {
            taken,
            agent_id: a.agent,
        },
    )
}
