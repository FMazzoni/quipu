//! Show the event log, for one task or across the store.
//!
#![doc = include_str!("../../docs/modules/timeline.md")]

use crate::outcome::Outcome;
use crate::store::{self, EventFilter};
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineArgs {
    #[serde(default)]
    pub task: Option<String>,
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
    #[arg(long = "kind")]
    #[serde(default)]
    pub kinds: Vec<String>,
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub since: i64,
}

impl crate::route::Served for TimelineArgs {
    const VERB: &'static str = "timeline";
    type Out = TimelineView;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

/// The event tail, already narrowed by `--since`, `--kind` and the task.
///
/// `--since` is an exclusive lower bound on the event id and it is a SQL
/// clause, so the steady-state answer to a poll is an empty array rather than
/// the whole log. That is the property `watch` will be built on: the cost of
/// tailing is proportional to what is new, not to what is stored.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct TimelineView(#[serde(deserialize_with = "body_is_an_array")] Value);


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
impl Default for TimelineView {
    fn default() -> Self {
        TimelineView(Value::Array(vec![]))
    }
}

impl Outcome for TimelineView {
    fn human(&self) -> String {
        self.0
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| {
                let kind = e["kind"].as_str().unwrap_or("");
                let body = crate::cmd::render::summarize_payload(kind, &e["payload"]);
                format!(
                    "{}\t{}\t{}\t{}",
                    e["ts"].as_str().unwrap_or(""),
                    kind,
                    e["agent_id"].as_str().unwrap_or("-"),
                    body
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn exec(db_path: &std::path::Path, a: TimelineArgs) -> Result<TimelineView> {
    let conn = db::open(db_path)?;
    let task_id = a
        .task
        .as_deref()
        .map(|s| id::resolve(&conn, s))
        .transpose()?;
    let filter = EventFilter {
        since_id: Some(a.since),
        task_id,
        kinds: &a.kinds,
        auto_only: false,
    };
    let rows = store::events(&conn, &filter)?;
    Ok(TimelineView(Value::Array(
        rows.iter().map(crate::cmd::render::event_json).collect(),
    )))
}
