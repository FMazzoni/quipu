//! Block until a cohort of tasks drains.
//!
#![doc = include_str!("../../docs/modules/wait.md")]

use crate::db;
use anyhow::{anyhow, Result};
use clap::Args;
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
    /// An empty cohort (no tasks match --tag) is a distinct error, exit code 4.
    #[arg(long)]
    pub cohort_done: bool,
    #[arg(long, default_value_t = 500)]
    pub interval_ms: u64,
    #[arg(long, default_value_t = 0)]
    pub timeout_secs: u64,
}

pub fn run(db_path: &std::path::Path, a: WaitArgs) -> Result<()> {
    if a.cohort_done {
        return run_cohort_done(db_path, &a);
    }
    if !a.empty {
        return Err(anyhow!("--empty is the only mode supported in MVP"));
    }
    let conn = db::open(db_path)?;
    let mut sql = String::from("SELECT COUNT(*) FROM task t WHERE 1=1");
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = a.state {
        sql.push_str(" AND t.state = ?");
        params.push(Box::new(s));
    }
    for tag in &a.tag {
        sql.push_str(" AND EXISTS (SELECT 1 FROM tag WHERE tag.task_id = t.id AND tag.name = ?)");
        params.push(Box::new(tag.clone()));
    }
    let mut stmt = conn.prepare(&sql)?;
    let start = Instant::now();
    loop {
        let pref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let n: i64 = stmt.query_row(pref.as_slice(), |r| r.get(0))?;
        if n == 0 {
            return Ok(());
        }
        if a.timeout_secs > 0 && start.elapsed() >= Duration::from_secs(a.timeout_secs) {
            // Distinct exit code 3 for timeout.
            std::process::exit(3);
        }
        std::thread::sleep(Duration::from_millis(a.interval_ms));
    }
}

/// `--cohort-done`: block until the tag-matched cohort has `total > 0` and
/// `non_terminal == 0` (non-terminal = state NOT IN ('done','cancelled')).
/// An empty cohort (`total == 0`) exits immediately with code 4 — it is
/// neither success nor an infinite block, since it usually signals a typo'd
/// tag or a race with `qp add`.
fn run_cohort_done(db_path: &std::path::Path, a: &WaitArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let mut sql = String::from(
        "SELECT COUNT(*), COUNT(CASE WHEN t.state NOT IN ('done','cancelled') THEN 1 END) \
         FROM task t WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    for tag in &a.tag {
        sql.push_str(" AND EXISTS (SELECT 1 FROM tag WHERE tag.task_id = t.id AND tag.name = ?)");
        params.push(Box::new(tag.clone()));
    }
    let mut stmt = conn.prepare(&sql)?;
    let start = Instant::now();
    loop {
        let pref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let (total, non_terminal): (i64, i64) =
            stmt.query_row(pref.as_slice(), |r| Ok((r.get(0)?, r.get(1)?)))?;
        if total == 0 {
            eprintln!("error: --cohort-done matched zero tasks for the given --tag filters");
            std::process::exit(4);
        }
        if non_terminal == 0 {
            return Ok(());
        }
        if a.timeout_secs > 0 && start.elapsed() >= Duration::from_secs(a.timeout_secs) {
            // Distinct exit code 3 for timeout.
            std::process::exit(3);
        }
        std::thread::sleep(Duration::from_millis(a.interval_ms));
    }
}
