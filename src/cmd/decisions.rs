//! Show decision events.
//!
#![doc = include_str!("../../docs/modules/decisions.md")]

use crate::db;
use crate::outcome::Outcome;
use crate::store::{self, EventFilter};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionsArgs {
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
    #[arg(long)]
    #[serde(default)]
    pub auto_only: bool,
    /// Exclusive lower bound on event id: `--since 730` starts at 731.
    /// Same semantics as `timeline --since`, because it is the same clause.
    #[arg(long)]
    #[serde(default)]
    pub since: Option<i64>,
}

impl crate::route::Served for DecisionsArgs {
    const VERB: &'static str = "decisions";
    type Out = DecisionsView;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

/// The decision events, already filtered by `--since` and `--auto-only`.
///
/// Both filters are SQL clauses on the server's `EventFilter`, so what crosses
/// the socket is the answer and never the store. A client that received
/// everything and trimmed would move the whole event log per poll.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct DecisionsView(#[serde(deserialize_with = "body_is_an_array")] Value);

/// Refuses a response body that is not the array this view renders.
///
/// The whole argument is on `wire::expect_array`; the short version is that a
/// `#[serde(transparent)]` newtype over `Value` accepts any JSON at all, and
/// every one of these views renders a mismatch as *empty* rather than as a
/// failure. `deserialize_with` keeps the payload a `Value` — so the bytes and
/// their key order are untouched — and adds the one refusal.
fn body_is_an_array<'de, D>(d: D) -> std::result::Result<Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    crate::wire::expect_array(Value::deserialize(d)?).map_err(D::Error::custom)
}

/// A minimal valid sample; see `list.rs` for why this is not derived.
#[cfg(test)]
impl Default for DecisionsView {
    fn default() -> Self {
        DecisionsView(Value::Array(vec![]))
    }
}

impl Outcome for DecisionsView {
    fn human(&self) -> String {
        self.0
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| {
                let auto = e["payload"]["auto"].as_bool().unwrap_or(false);
                let text = e["payload"]["text"].as_str().unwrap_or("");
                let flag = if auto { "[auto]" } else { "      " };
                format!(
                    "{}\t{}\t{}\t{}\t{}",
                    e["ts"].as_str().unwrap_or(""),
                    e["task"].as_str().unwrap_or("-"),
                    e["agent_id"].as_str().unwrap_or("-"),
                    flag,
                    text
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn exec(db_path: &std::path::Path, a: DecisionsArgs) -> Result<DecisionsView> {
    let conn = db::open(db_path)?;
    let kinds = ["decision".to_string()];
    let filter = EventFilter {
        since_id: a.since,
        task_id: None,
        kinds: &kinds,
        auto_only: a.auto_only,
    };
    let rows = store::events(&conn, &filter)?;
    Ok(DecisionsView(Value::Array(
        rows.iter().map(crate::cmd::render::event_json).collect(),
    )))
}
