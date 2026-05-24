use anyhow::Result;
use clap::Args;
use crate::cmd::timeline::{run as run_timeline, TimelineArgs};

#[derive(Args, Debug)]
pub struct DecisionsArgs {
    #[arg(long)] pub json: bool,
    #[arg(long)] pub auto_only: bool,
}

pub fn run(db_path: &std::path::Path, a: DecisionsArgs) -> Result<()> {
    // auto_only post-filter is a known limitation (documented in README).
    let _ = a.auto_only;
    run_timeline(db_path, TimelineArgs {
        task: None, json: a.json, kinds: vec!["decision".into()], since: 0,
    })
}
