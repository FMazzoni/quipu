//! Snapshot of task counts per state.

use crate::db;
use crate::outcome::Outcome;
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Every state, in the order both the JSON fill-in and the prose use.
const STATES: &[&str] = &[
    "pending",
    "ready",
    "assigned",
    "running",
    "done",
    "cancelled",
];

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusArgs {
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
}

impl crate::route::Served for StatusArgs {
    const VERB: &'static str = "status";
    type Out = StatusView;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

/// The state → count object, with every state present even at zero.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct StatusView(Value);

impl Outcome for StatusView {
    fn human(&self) -> String {
        STATES
            .iter()
            .map(|s| {
                let count = self.0.get(*s).and_then(Value::as_i64).unwrap_or(0);
                format!("{s:<10} {count}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn exec(db_path: &std::path::Path, _a: StatusArgs) -> Result<StatusView> {
    let conn = db::open(db_path)?;
    let mut stmt =
        conn.prepare("SELECT state, COUNT(*) FROM task GROUP BY state ORDER BY state")?;
    let rows: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut map = serde_json::Map::new();
    for (state, count) in &rows {
        map.insert(state.clone(), Value::from(*count));
    }
    for s in STATES {
        map.entry((*s).to_string()).or_insert(Value::from(0));
    }
    Ok(StatusView(Value::Object(map)))
}
