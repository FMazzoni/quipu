//! Block until a cohort of tasks drains.
//!
#![doc = include_str!("../../docs/modules/wait.md")]

use crate::poll::Interval;
use crate::route::{self, Ctx};
use crate::{cmd::list::ListArgs, db};
use anyhow::{anyhow, Result};
use clap::Args;
use serde_json::Value;
use std::time::{Duration, Instant};

#[derive(Args, Debug)]
pub struct WaitArgs {
    #[arg(long)]
    pub tag: Vec<String>,
    #[arg(long)]
    pub state: Option<db::State>,
    #[arg(long)]
    pub empty: bool,
    /// Block until the tag-matched cohort has drained: total > 0 and no task
    /// is left in a non-terminal state (state NOT IN ('done','cancelled')).
    /// An empty cohort (no tasks match `--tag`) is a distinct error, exit code 4.
    #[arg(long)]
    pub cohort_done: bool,
    #[arg(long, default_value_t = 500)]
    pub interval_ms: u64,
    /// Ceiling the interval backs off to while the cohort is quiet. Defaults to
    /// ten times `--interval-ms`; set it equal to `--interval-ms` to disable.
    #[arg(long)]
    pub max_interval_ms: Option<u64>,
    #[arg(long, default_value_t = 0)]
    pub timeout_secs: u64,
}

/// The two numbers every mode of this command is a predicate over.
///
/// Both come from one `list` — which is one SQL query locally and one round trip
/// through a front door — rather than from a `COUNT`, because the served read is
/// already filtered by `--state` and `--tag` on the store side and a second
/// endpoint for one caller is a protocol surface nobody measured a need for.
struct Cohort {
    total: usize,
    non_terminal: usize,
}

impl Cohort {
    fn of(rows: &Value) -> Self {
        let rows = rows.as_array().map(Vec::as_slice).unwrap_or_default();
        Cohort {
            total: rows.len(),
            non_terminal: rows
                .iter()
                .filter(|r| !matches!(r["state"].as_str(), Some("done") | Some("cancelled")))
                .count(),
        }
    }
}

pub fn run(ctx: &Ctx, a: WaitArgs) -> Result<()> {
    if !a.empty && !a.cohort_done {
        return Err(anyhow!("--empty is the only mode supported in MVP"));
    }
    let start = Instant::now();
    let mut interval = Interval::new(a.interval_ms, a.max_interval_ms);
    let mut last: Option<(usize, usize)> = None;
    loop {
        // A fresh `ListArgs` per poll: it is consumed by `exec`, and the server
        // side wants the request body rebuilt anyway. `--state` is deliberately
        // dropped in `--cohort-done`, which ignores it exactly as it always has.
        let rows = route::fetch::<ListArgs>(
            ctx,
            ListArgs {
                assigned_to: None,
                state: if a.cohort_done { None } else { a.state },
                tag: a.tag.clone(),
                json: false,
                with_description: false,
            },
        )?;
        let c = Cohort::of(rows.rows());

        if a.cohort_done {
            if c.total == 0 {
                eprintln!("error: --cohort-done matched zero tasks for the given --tag filters");
                std::process::exit(4);
            }
            if c.non_terminal == 0 {
                return Ok(());
            }
        } else if c.total == 0 {
            return Ok(());
        }

        if a.timeout_secs > 0 && start.elapsed() >= Duration::from_secs(a.timeout_secs) {
            // Distinct exit code 3 for timeout.
            std::process::exit(3);
        }
        match last.replace((c.total, c.non_terminal)) {
            Some(prev) if prev == (c.total, c.non_terminal) => interval.saw_nothing(),
            _ => interval.saw_change(),
        }
        interval.wait();
    }
}
