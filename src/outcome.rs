//! Shared success-output plumbing for mutating commands.
//!
//! Every mutation models its result once as a small `Outcome` struct
//! (`Serialize` for `--json`, `human()` for the prose line printed today),
//! then renders it through `emit`. This generalizes the pattern already used
//! by `qp add`'s `Created` struct.

/// A command's success payload. `human()` is the one-line prose summary;
/// `Serialize` gives the bare JSON object (no `{"ok":true,...}` wrapper —
/// success is already disjoint from error by stream (stdout) and exit code).
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
