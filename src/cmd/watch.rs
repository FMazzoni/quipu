//! Tail new events as they are recorded.
//!
#![doc = include_str!("../../docs/modules/watch.md")]

use crate::cmd::timeline::TimelineArgs;
use crate::poll::Interval;
use crate::route::{self, Ctx};
use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct WatchArgs {
    /// Start tailing from this event id (exclusive).
    #[arg(long, default_value_t = 0)]
    pub since: i64,
    /// Poll interval in milliseconds.
    #[arg(long, default_value_t = 500)]
    pub interval_ms: u64,
    /// Ceiling the interval backs off to while the log is quiet. Defaults to
    /// ten times `--interval-ms`; set it equal to `--interval-ms` to disable.
    #[arg(long)]
    pub max_interval_ms: Option<u64>,
    /// Emit JSON lines (default true since watch is for agents).
    #[arg(long, default_value_t = true)]
    pub json: bool,
    /// Stop after N empty-or-non-empty polling ticks. 0 = forever.
    #[arg(long, default_value_t = 0)]
    pub max_ticks: u64,
    /// Filter to events for this task only.
    #[arg(long)]
    pub task: Option<String>,
    /// Filter to these event kinds (repeatable).
    #[arg(long = "kind")]
    pub kinds: Vec<String>,
}

pub fn run(ctx: &Ctx, a: WatchArgs) -> Result<()> {
    let mut last_seen = a.since;
    let mut ticks: u64 = 0;
    let mut interval = Interval::new(a.interval_ms, a.max_interval_ms);
    loop {
        // One `timeline` per tick, narrowed by `since` on the store side, so a
        // caught-up poller transfers an empty array rather than the log.
        // Materialised, never streamed: the rows are a `Vec` before the response
        // exists, so no read cursor can outlive it and pin a WAL checkpoint.
        let view = route::fetch::<TimelineArgs>(
            ctx,
            TimelineArgs {
                task: a.task.clone(),
                json: false,
                kinds: a.kinds.clone(),
                since: last_seen,
            },
        )?;
        let events = view
            .events()
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        for e in events {
            if a.json {
                println!("{}", serde_json::to_string(e)?);
            } else {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    e["ts"].as_str().unwrap_or(""),
                    e["task"].as_str().unwrap_or("-"),
                    e["kind"].as_str().unwrap_or(""),
                    e["agent_id"].as_str().unwrap_or("-"),
                    e["payload"]
                );
            }
            if let Some(id) = e["id"].as_i64() {
                last_seen = last_seen.max(id);
            }
        }
        ticks += 1;
        if a.max_ticks > 0 && ticks >= a.max_ticks {
            return Ok(());
        }
        if events.is_empty() {
            interval.saw_nothing();
        } else {
            interval.saw_change();
        }
        interval.wait();
    }
}
