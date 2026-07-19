use crate::outcome::{emit, Outcome};
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct DependsArgs {
    /// Task that depends on another (the upstream).
    pub task: String,
    /// Task that `task` depends on (the prerequisite).
    #[arg(long = "on")]
    pub on: String,
    /// Remove the dep edge instead of adding it.
    #[arg(long)]
    pub rm: bool,
    /// Required when `task` is assigned/running. Must match the latest assignee.
    #[arg(long = "as")]
    pub agent: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct DependsOutcome {
    display_id: String,
    on_id: String,
    op: &'static str,
    /// For `rm`: did removing this edge promote `display_id` itself to `ready`?
    /// Always `false` for `add` (adding an edge can only demote, never promote).
    promoted: bool,
}
impl Outcome for DependsOutcome {
    fn human(&self) -> String {
        let verb = if self.op == "rm" {
            "unlinked"
        } else {
            "linked"
        };
        format!("{} {} {}", self.display_id, verb, self.on_id)
    }
}

pub fn run(db_path: &std::path::Path, a: DependsArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_resolved = id::resolve_full(&conn, &a.task)?;
    let on_resolved = id::resolve_full(&conn, &a.on)?;
    let task_id = task_resolved.id;
    let on_id = on_resolved.id;
    let promoted = db::with_tx(&mut conn, |tx| -> Result<bool> {
        // Ownership gate: if the downstream task (the one gaining the dep) is
        // assigned/running, --as must match the assignee. We guard the downstream
        // because that is the row being mutated; the upstream is unchanged.
        let downstream_state: String =
            tx.query_row("SELECT state FROM task WHERE id = ?", [task_id], |r| {
                r.get(0)
            })?;
        if matches!(downstream_state.as_str(), "assigned" | "running") {
            let assignee: Option<String> = db::current_assignment(tx, task_id)?.map(|o| o.agent_id);
            match (a.agent.as_deref(), assignee.as_deref()) {
                (Some(want), Some(have)) if want == have => {}
                _ => {
                    return Err(db::not_owner(
                        format!(
                            "{} is {downstream_state}; --as must match latest assignee",
                            a.task
                        ),
                        Some(task_resolved.display_id.clone()),
                        assignee,
                    ))
                }
            }
        }

        if a.rm {
            let n = tx.execute(
                "DELETE FROM dep WHERE task_id = ? AND depends_on_task_id = ?",
                rusqlite::params![task_id, on_id],
            )?;
            if n == 0 {
                return Err(db::not_found(
                    format!("no dep {} → {}", a.task, a.on),
                    Some(task_resolved.display_id.clone()),
                ));
            }
            db::insert_event(
                tx,
                Some(task_id),
                "dep_removed",
                a.agent.as_deref(),
                Some(&serde_json::json!({"on": a.on})),
            )?;
            // Snapshot pending tasks whose deps are now all resolved (candidates for promotion).
            let promoted_ids: Vec<i64> = {
                let mut stmt = tx.prepare(
                    "SELECT t.id FROM task t WHERE t.state = 'pending' \
                      AND NOT EXISTS (SELECT 1 FROM dep d JOIN task t2 ON t2.id = d.depends_on_task_id \
                                       WHERE d.task_id = t.id AND t2.state NOT IN ('done','cancelled'))")?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            db::refresh_ready(tx)?;
            let mut self_promoted = false;
            for tid in promoted_ids {
                let now: String =
                    tx.query_row("SELECT state FROM task WHERE id = ?", [tid], |r| r.get(0))?;
                if now == "ready" {
                    if tid == task_id {
                        self_promoted = true;
                    }
                    db::insert_event(
                        tx,
                        Some(tid),
                        "state_change",
                        a.agent.as_deref(),
                        Some(&serde_json::json!({"to": "ready", "via": "depends_rm"})),
                    )?;
                }
            }
            Ok(self_promoted)
        } else {
            if db::would_cycle(tx, task_id, on_id)? {
                return Err(db::invariant(
                    "dependency_cycle",
                    format!(
                        "cycle: {} depends on {} which (transitively) depends on {}",
                        a.task, a.on, a.task
                    ),
                ));
            }
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO dep(task_id, depends_on_task_id) VALUES (?,?)",
                rusqlite::params![task_id, on_id],
            )?;
            if inserted == 0 {
                // Already present — treat as idempotent success.
                return Ok(false);
            }
            // If the upstream was `ready` and the new dep is unresolved, demote to pending.
            // Idempotent guarded UPDATE — matches only ready tasks with an unresolved dep.
            let demoted = tx.execute(
                "UPDATE task SET state = ?1
                  WHERE id = ?2 AND state = ?3
                    AND EXISTS (SELECT 1 FROM dep d JOIN task t2 ON t2.id = d.depends_on_task_id
                                 WHERE d.task_id = ?2 AND t2.state NOT IN ('done','cancelled'))",
                rusqlite::params![db::State::Pending, task_id, db::State::Ready],
            )?;
            if demoted == 1 {
                db::insert_event(
                    tx,
                    Some(task_id),
                    "state_change",
                    a.agent.as_deref(),
                    Some(&serde_json::json!({"to": "pending", "via": "depends"})),
                )?;
            }
            db::insert_event(
                tx,
                Some(task_id),
                "dep_added",
                a.agent.as_deref(),
                Some(&serde_json::json!({"on": a.on})),
            )?;
            Ok(false)
        }
    })?;
    let op = if a.rm { "rm" } else { "add" };
    emit(
        a.json,
        &DependsOutcome {
            display_id: task_resolved.display_id,
            on_id: on_resolved.display_id,
            op,
            promoted,
        },
    )
}
