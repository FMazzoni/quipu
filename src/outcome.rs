//! Shared success-output plumbing for mutating commands.
//!
#![doc = include_str!("../docs/modules/outcome.md")]

/// A command's success payload.
///
/// `human()` is the one-line prose summary; `Serialize` gives the bare JSON
/// object (no `{"ok":true,...}` wrapper — success is already disjoint from
/// error by stream (stdout) and exit code).
pub trait Outcome: serde::Serialize {
    fn human(&self) -> String;
}

/// Print `o` as JSON (if `json`) or its human summary, to stdout.
pub fn emit(json: bool, o: &impl Outcome) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string(o)?);
    } else {
        println!("{}", o.human());
    }
    Ok(())
}
