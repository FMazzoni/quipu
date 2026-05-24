//! `qp edit` — mutate task fields (title, tier, description). Emits one `edit` event.
//! Stub created during Phase 0 scaffold. Body implemented in Pattern D wave.

use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct EditArgs {
    pub task: String,
    #[arg(long)] pub title: Option<String>,
    #[arg(long)] pub tier: Option<String>,
    #[arg(long)] pub description: Option<String>,
    #[arg(long = "as")] pub agent: Option<String>,
}

pub fn run(_db_path: &std::path::Path, _a: EditArgs) -> Result<()> {
    anyhow::bail!("not yet implemented (Phase 0 scaffold)")
}
