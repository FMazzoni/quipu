//! Attach a store to a ledger, or detach it back to local-only.
//!
#![doc = include_str!("../../docs/modules/remote.md")]

use crate::db;
use crate::outcome::{emit, Outcome};
use crate::remote::{self, Reach, Remotes};
use anyhow::Result;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Args, Debug)]
pub struct RemoteArgs {
    #[command(subcommand)]
    pub op: RemoteOp,
}

#[derive(Subcommand, Debug)]
pub enum RemoteOp {
    /// Point this store at a ledger (config only)
    Set {
        /// Object-store url the ledger lives at: `s3://<host>/<bucket>[/<prefix>]`.
        #[arg(long, value_name = "URL")]
        ledger: Option<String>,
        /// Also probe the url. Reports reachability; never blocks the write.
        #[arg(long)]
        check: bool,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Report what this store is attached to
    Show {
        /// Probe each configured url. Off by default, so `show` stays local and instant.
        #[arg(long)]
        check: bool,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Diagnose what is configured and name the cause of any failure
    Doctor {
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Detach — the deliberate route back to local-only operation
    Clear {
        /// Clear only the ledger.
        #[arg(long)]
        ledger: bool,
        /// Clear both. The default when no flag is given.
        #[arg(long)]
        all: bool,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
}

impl RemoteArgs {
    /// Whether `--json` was passed on whichever subcommand was chosen.
    ///
    /// Lets `main`'s error path match the stream format the success path uses.
    pub fn json(&self) -> bool {
        match &self.op {
            RemoteOp::Set { json, .. }
            | RemoteOp::Show { json, .. }
            | RemoteOp::Doctor { json }
            | RemoteOp::Clear { json, .. } => *json,
        }
    }
}

/// Exit code when `doctor` finds a fault. Documented in `docs/architecture.md`.
const EXIT_FAULT: i32 = 5;

/// Reachability as it appears in output: absent unless `--check` asked for it.
#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct Checked {
    /// The field name carries the `_reachable` suffix because `Checked` is
    /// `#[serde(flatten)]`ed next to the url fields; bare `ledger` would
    /// emit the key twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ledger_reachable: Option<Reach>,
}

impl Checked {
    fn line(label: &str, r: Option<&Reach>) -> Option<String> {
        match r? {
            Reach::Ok => Some(format!("{label}: reachable")),
            Reach::Fault(why) => Some(format!("{label}: UNUSABLE — {why}")),
        }
    }
    fn lines(&self) -> Vec<String> {
        [Self::line("ledger", self.ledger_reachable.as_ref())]
            .into_iter()
            .flatten()
            .collect()
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct RemoteSet {
    ledger: Option<String>,
    /// Which keys this invocation wrote. A list, not a bool: `set` has taken
    /// more than one url before and will again, and a caller that branches on
    /// the names keeps working when it does.
    changed: Vec<String>,
    #[serde(flatten)]
    checked: Checked,
}
impl Outcome for RemoteSet {
    fn human(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        for k in &self.changed {
            out.push(format!(
                "{k} set to {}",
                self.ledger.as_deref().unwrap_or("")
            ));
        }
        out.extend(self.checked.lines());
        out.join("\n")
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct RemoteShow {
    /// This store's own id, and the project it belongs to. Shown here because
    /// this is the command that answers "who am I to the other machines" — and
    /// two stores of one project differ in the first and agree in the second.
    project_uuid: String,
    project_key: String,
    ledger: Option<String>,
    /// True when no url is set. The default state, and a supported one.
    local_only: bool,
    #[serde(flatten)]
    checked: Checked,
}
impl Outcome for RemoteShow {
    fn human(&self) -> String {
        let mut out = vec![
            format!("project:   {}", self.project_key),
            format!("store:     {}", self.project_uuid),
        ];
        if self.local_only {
            out.push("local-only — no ledger".to_string());
            return out.join("\n");
        }
        out.push(format!(
            "ledger:    {}",
            self.ledger.as_deref().unwrap_or("(unset)")
        ));
        out.extend(self.checked.lines());
        out.join("\n")
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct RemoteCleared {
    /// Keys that actually held a value. Clearing an unset key is a no-op, not an error.
    cleared: Vec<String>,
    ledger: Option<String>,
    local_only: bool,
}
impl Outcome for RemoteCleared {
    fn human(&self) -> String {
        let what = if self.cleared.is_empty() {
            "nothing to clear".to_string()
        } else {
            format!("{} cleared", self.cleared.join(", "))
        };
        if self.local_only {
            format!("{what} — this store is now local-only")
        } else {
            what
        }
    }
}

pub fn run(db_path: &std::path::Path, a: RemoteArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    match a.op {
        RemoteOp::Set {
            ledger,
            check,
            json,
        } => {
            let Some(url) = &ledger else {
                return Err(db::invalid_input("nothing to set: pass --ledger <url>"));
            };
            remote::validate_ledger_url(url)?;
            remote::set(&conn, remote::KEY_LEDGER, url)?;
            let changed = vec!["ledger"];
            let now = remote::load(&conn)?;
            let checked = probe_all(&now, check, &changed);
            emit(
                json,
                &RemoteSet {
                    ledger: now.ledger,
                    changed: changed.into_iter().map(str::to_string).collect(),
                    checked,
                },
            )
        }
        RemoteOp::Show { check, json } => {
            let now = remote::load(&conn)?;
            let checked = probe_all(&now, check, &["ledger"]);
            emit(
                json,
                &RemoteShow {
                    project_uuid: crate::project::store_uuid(&conn)?,
                    project_key: crate::project::require(&conn)?,
                    local_only: now.local_only(),
                    ledger: now.ledger,
                    checked,
                },
            )
        }
        RemoteOp::Doctor { json } => {
            let report = crate::doctor::run(db_path, &remote::load(&conn)?);
            let failed = report.failed();
            emit(json, &report)?;
            // The diagnosis is the output, not the error: the command did what
            // it was asked whatever it found, so the report goes to stdout in
            // the requested format rather than through the error envelope. The
            // exit code is what a setup script branches on, and it gets a code
            // of its own — reusing the retryable-conflict code would make a
            // skill that retries on it loop forever against a broken remote.
            if failed {
                std::process::exit(EXIT_FAULT);
            }
            Ok(())
        }
        RemoteOp::Clear { ledger, all, json } => {
            // Every flag combination clears the same single url today. They are
            // kept because `clear` is the escape hatch: a script that already
            // says `--all`, or names the url it means, must not start failing
            // just because there is currently only one thing to clear.
            let _ = (ledger, all);
            let mut cleared: Vec<&'static str> = Vec::new();
            if remote::clear(&conn, remote::KEY_LEDGER)? {
                cleared.push("ledger");
            }
            let now = remote::load(&conn)?;
            emit(
                json,
                &RemoteCleared {
                    cleared: cleared.into_iter().map(str::to_string).collect(),
                    local_only: now.local_only(),
                    ledger: now.ledger,
                },
            )
        }
    }
}

/// Probes only what `--check` asked for and only what is configured.
///
/// Without `--check` this returns two `None`s and spawns nothing, which is what
/// holds `qp remote` to the same no-network promise as the rest of the CLI.
fn probe_all(r: &Remotes, check: bool, which: &[&'static str]) -> Checked {
    if !check {
        return Checked {
            ledger_reachable: None,
        };
    }
    let one = |key: &str, url: &Option<String>| -> Option<Reach> {
        if !which.contains(&key) {
            return None;
        }
        url.as_deref().map(remote::probe)
    };
    Checked {
        ledger_reachable: one("ledger", &r.ledger),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flattened `Checked` with its keys present.
    ///
    /// A `#[serde(flatten)]`ed field with every key absent — the registry's
    /// `Default` sample — is a different deserialisation path from this one.
    #[test]
    fn a_probed_remote_round_trips() {
        let checked = || Checked {
            authority_reachable: Some(Reach::Ok),
            ledger_reachable: Some(Reach::Fault("bucket not found".to_string())),
        };
        crate::outcome::round_trip::assert_round_trips(&RemoteSet {
            authority: Some("https://git.example.com/q.git".to_string()),
            ledger: Some("s3://example/ledger".to_string()),
            changed: vec!["authority".to_string(), "ledger".to_string()],
            checked: checked(),
        });
        crate::outcome::round_trip::assert_round_trips(&RemoteShow {
            project_uuid: "00000000-0000-4000-8000-000000000000".to_string(),
            project_key: "example".to_string(),
            authority: None,
            ledger: Some("s3://example/ledger".to_string()),
            local_only: false,
            checked: checked(),
        });
    }
}
