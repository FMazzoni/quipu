//! `qp depends` — add or remove a dep edge between tasks. Cycle-checked.
//! Stub created during Phase 0 scaffold. Body implemented in Task A1.

use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct DependsArgs {
    pub task: String,
    #[arg(long = "on")] pub on: String,
    #[arg(long)] pub rm: bool,
    #[arg(long = "as")] pub agent: Option<String>,
}

pub fn run(_db_path: &std::path::Path, _a: DependsArgs) -> Result<()> {
    anyhow::bail!("not yet implemented (Phase 0 scaffold)")
}
