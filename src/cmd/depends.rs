use anyhow::Result;
use clap::Args;
use crate::{db, id};

#[derive(Args, Debug)]
pub struct DependsArgs {
    /// Task that depends on another (the upstream).
    pub task: String,
    /// Task that `task` depends on (the prerequisite).
    #[arg(long = "on")] pub on: String,
    /// Remove the dep edge instead of adding it.
    #[arg(long)] pub rm: bool,
    /// Required when `task` is assigned/running. Must match the latest assignee.
    #[arg(long = "as")] pub agent: Option<String>,
}

pub fn run(db_path: &std::path::Path, a: DependsArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_id = id::resolve(&conn, &a.task)?;
    let on_id   = id::resolve(&conn, &a.on)?;
    db::with_tx(&mut conn, |tx| {
        // Ownership gate: if upstream is assigned/running, --as must match.
        let upstream_state: String = tx.query_row(
            "SELECT state FROM task WHERE id = ?", [task_id], |r| r.get(0))?;
        if matches!(upstream_state.as_str(), "assigned" | "running") {
            let assignee: Option<String> = tx.query_row(
                "SELECT agent_id FROM assignment WHERE task_id = ? ORDER BY id DESC LIMIT 1",
                [task_id], |r| r.get(0)).ok();
            match (a.agent.as_deref(), assignee.as_deref()) {
                (Some(want), Some(have)) if want == have => {}
                _ => return Err(db::constraint(format!(
                    "{} is {upstream_state}; --as must match latest assignee", a.task))),
            }
        }

        if a.rm {
            let n = tx.execute(
                "DELETE FROM dep WHERE task_id = ? AND depends_on_task_id = ?",
                rusqlite::params![task_id, on_id])?;
            if n == 0 {
                return Err(db::constraint(format!("no dep {} → {}", a.task, a.on)));
            }
            db::insert_event(tx, Some(task_id), "dep_removed", a.agent.as_deref(),
                Some(&serde_json::json!({"on": a.on})))?;
            db::refresh_ready(tx)?;
        } else {
            if db::would_cycle(tx, task_id, on_id)? {
                return Err(db::constraint(format!(
                    "cycle: {} depends on {} which (transitively) depends on {}",
                    a.task, a.on, a.task)));
            }
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO dep(task_id, depends_on_task_id) VALUES (?,?)",
                rusqlite::params![task_id, on_id])?;
            if inserted == 0 {
                // Already present — treat as idempotent success.
                return Ok(());
            }
            // If the upstream was `ready` and the new dep is unresolved, demote to pending.
            // Idempotent guarded UPDATE — matches only ready tasks with an unresolved dep.
            let demoted = tx.execute(
                "UPDATE task SET state = 'pending'
                  WHERE id = ?1 AND state = 'ready'
                    AND EXISTS (SELECT 1 FROM dep d JOIN task t2 ON t2.id = d.depends_on_task_id
                                 WHERE d.task_id = ?1 AND t2.state NOT IN ('done','cancelled'))",
                [task_id])?;
            if demoted == 1 {
                db::insert_event(tx, Some(task_id), "state_change", a.agent.as_deref(),
                    Some(&serde_json::json!({"to": "pending", "via": "depends"})))?;
            }
            db::insert_event(tx, Some(task_id), "dep_added", a.agent.as_deref(),
                Some(&serde_json::json!({"on": a.on})))?;
        }
        Ok(())
    })?;
    let verb = if a.rm { "unlinked" } else { "linked" };
    println!("{} {} {}", a.task.to_uppercase(), verb, a.on.to_uppercase());
    Ok(())
}
