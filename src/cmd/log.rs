//! Record a free-form event against a task.
//!
#![doc = include_str!("../../docs/modules/log.md")]

use crate::outcome::Outcome;
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogArgs {
    pub task: String,
    pub kind: String,
    pub body: String,
    #[arg(long = "as")]
    #[serde(default)]
    pub agent: Option<String>,
    #[arg(long)]
    #[serde(default)]
    pub auto: bool,
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
}

impl crate::route::Served for LogArgs {
    const VERB: &'static str = "log";
    type Out = Logged;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct Logged {
    display_id: String,
    kind: String,
    agent: Option<String>,
}
impl Outcome for Logged {
    fn human(&self) -> String {
        format!("logged {} on {}", self.kind, self.display_id)
    }
}

pub fn exec(db_path: &std::path::Path, a: LogArgs) -> Result<Logged> {
    let mut conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let task_id = resolved.id;
    let attributed_agent = db::with_tx(&mut conn, |tx| -> Result<Option<String>> {
        let mut payload = serde_json::json!({"text": a.body});
        if a.auto {
            payload["auto"] = serde_json::Value::Bool(true);
        }
        let auto_agent: Option<String> = if a.agent.is_none() {
            let state: Option<String> = tx
                .query_row("SELECT state FROM task WHERE id = ?", [task_id], |r| {
                    r.get(0)
                })
                .ok();
            if state.as_deref() == Some("running") {
                tx.query_row(
                    "SELECT agent_id FROM assignment
                      WHERE task_id = ? AND completed_at IS NULL
                      ORDER BY id DESC LIMIT 1",
                    [task_id],
                    |r| r.get(0),
                )
                .ok()
            } else {
                None
            }
        } else {
            None
        };
        let agent = a.agent.clone().or(auto_agent);
        db::insert_event(tx, Some(task_id), &a.kind, agent.as_deref(), Some(&payload))?;
        Ok(agent)
    })?;
    Ok(Logged {
        display_id: resolved.display_id,
        kind: a.kind,
        agent: attributed_agent,
    })
}
