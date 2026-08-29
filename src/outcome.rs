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
///
/// Empty prose prints *nothing*, not a blank line. Every mutating payload
/// renders one line unconditionally, so this arm only fires on the read views,
/// where it is what those commands always did: `qp timeline` on a store with no
/// events wrote no bytes at all, and a blank line would be a new one for every
/// consumer that counts them.
pub fn emit(json: bool, o: &impl Outcome) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string(o)?);
    } else {
        let text = o.human();
        if !text.is_empty() {
            println!("{text}");
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod round_trip {
    use super::Outcome;

    /// Asserts a payload survives `serialize` → `deserialize` → `human()`.
    ///
    /// The prose is rendered from the *reconstituted* value, which is the only
    /// thing that proves a caller holding nothing but the JSON can produce the
    /// same sentence the store-side process would have.
    pub(crate) fn assert_round_trips<T>(o: &T)
    where
        T: Outcome + serde::de::DeserializeOwned,
    {
        let name = std::any::type_name::<T>();
        let json = serde_json::to_string(o).unwrap_or_else(|e| panic!("{name} serialises: {e}"));
        let back: T = serde_json::from_str(&json).unwrap_or_else(|e| {
            panic!(
                "{name} does not survive its own JSON: {e}\n  payload: {json}\n  \
                 a field with `skip_serializing_if` needs `#[serde(default)]` too"
            )
        });
        assert_eq!(
            o.human(),
            back.human(),
            "{name} renders differently after a round trip"
        );
        assert_eq!(
            json,
            serde_json::to_string(&back).unwrap(),
            "{name} is not fixed by a round trip"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::round_trip::assert_round_trips;
    use crate::cmd::abandon::Abandoned;
    use crate::cmd::add::Created;
    use crate::cmd::assign::Assigned;
    use crate::cmd::block::Blocked;
    use crate::cmd::cancel::Cancelled;
    use crate::cmd::claim::Claimed;
    use crate::cmd::complete::Completed;
    use crate::cmd::contains::ContainsOutcome;
    use crate::cmd::decisions::DecisionsView;
    use crate::cmd::depends::DependsOutcome;
    use crate::cmd::edit::Edited;
    use crate::cmd::export::ExportOutcome;
    use crate::cmd::import::ImportOutcome;
    use crate::cmd::init::InitOutcome;
    use crate::cmd::ledger::LedgerOutcome;
    use crate::cmd::list::ListView;
    use crate::cmd::log::Logged;
    use crate::cmd::project::{ListOutcome, LookupOutcome, RegisterOutcome, UnregisterOutcome};
    use crate::cmd::reclaim::Reclaimed;
    use crate::cmd::relation::{RelationAdded, RelationList, RelationRemoved};
    use crate::cmd::remote::{RemoteCleared, RemoteSet, RemoteShow};
    use crate::cmd::report::ReportView;
    use crate::cmd::show::ShowView;
    use crate::cmd::status::StatusView;
    use crate::cmd::sync::SyncOutcome;
    use crate::cmd::tag::Tagged;
    use crate::cmd::take::Taken;
    use crate::cmd::timeline::TimelineView;
    use crate::cmd::tree::TreeView;
    use crate::cmd::wave::WaveView;
    use crate::doctor::Report;

    /// Names every payload once, for both halves of the property.
    ///
    /// `REGISTERED` is what `the_registry_is_every_payload` compares the source
    /// scan against, so the list cannot quietly fall behind the tree.
    macro_rules! payloads {
        ($($t:ident),+ $(,)?) => {
            const REGISTERED: &[&str] = &[$(stringify!($t)),+];

            #[test]
            fn every_payload_round_trips() {
                $( assert_round_trips(&<$t as Default>::default()); )+
            }
        };
    }

    payloads!(
        Abandoned,
        Assigned,
        Blocked,
        Cancelled,
        Claimed,
        Completed,
        ContainsOutcome,
        Created,
        DecisionsView,
        DependsOutcome,
        Edited,
        ExportOutcome,
        ImportOutcome,
        InitOutcome,
        LedgerOutcome,
        ListOutcome,
        ListView,
        Logged,
        LookupOutcome,
        Reclaimed,
        RegisterOutcome,
        RelationAdded,
        RelationList,
        RelationRemoved,
        RemoteCleared,
        RemoteSet,
        RemoteShow,
        Report,
        ReportView,
        ShowView,
        StatusView,
        SyncOutcome,
        Tagged,
        Taken,
        TimelineView,
        TreeView,
        UnregisterOutcome,
        WaveView,
    );

    /// Every `impl Outcome` in the tree is in `REGISTERED`, and nothing else is.
    ///
    /// This is the half that survives a new command. Rust has no reflection, so
    /// the registry above is written by hand; scanning the source for the impls
    /// is what stops the hand-written half from rotting the way a checklist in
    /// a comment does. The scan reads source rather than rendered anything, for
    /// the reason `tests/docs.rs` gives.
    #[test]
    fn the_registry_is_every_payload() {
        let mut found: Vec<String> = Vec::new();
        let mut files = Vec::new();
        collect(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        for f in &files {
            for line in std::fs::read_to_string(f).unwrap().lines() {
                let line = line.trim_start();
                let Some(rest) = line
                    .strip_prefix("impl Outcome for ")
                    .or_else(|| line.strip_prefix("impl crate::outcome::Outcome for "))
                else {
                    continue;
                };
                found.push(rest.trim_end_matches(" {").to_string());
            }
        }
        found.sort();
        let mut registered: Vec<String> = REGISTERED.iter().map(|s| s.to_string()).collect();
        registered.sort();
        assert_eq!(
            found, registered,
            "a payload was added or removed without updating `REGISTERED` in \
             `outcome.rs` — every `impl Outcome` must round-trip, which needs \
             `Deserialize`, `pub(crate)`, and an entry in the registry"
        );
        assert!(
            found.len() >= 38,
            "scan found only {} impls — did the scan break?",
            found.len()
        );
    }

    fn collect(dir: std::path::PathBuf, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                collect(p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
}
