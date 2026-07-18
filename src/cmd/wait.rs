use crate::db;
use anyhow::{anyhow, Result};
use clap::Args;
use std::time::{Duration, Instant};

#[derive(Args, Debug)]
pub struct WaitArgs {
    #[arg(long)]
    pub tag: Vec<String>,
    #[arg(long)]
    pub state: Option<String>,
    #[arg(long)]
    pub empty: bool,
    #[arg(long, default_value_t = 500)]
    pub interval_ms: u64,
    #[arg(long, default_value_t = 0)]
    pub timeout_secs: u64,
}

pub fn run(db_path: &std::path::Path, a: WaitArgs) -> Result<()> {
    if !a.empty {
        return Err(anyhow!("--empty is the only mode supported in MVP"));
    }
    let conn = db::open(db_path)?;
    let mut sql = String::from("SELECT COUNT(*) FROM task t WHERE 1=1");
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = &a.state {
        sql.push_str(" AND t.state = ?");
        params.push(Box::new(s.clone()));
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
