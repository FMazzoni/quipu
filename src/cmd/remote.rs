//! Attach a store to a ledger or a front door, or detach it back to local-only.
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
    /// Point this store at a ledger or a front door (config only)
    Set {
        /// Object-store url the ledger lives at: `s3://<host>/<bucket>[/<prefix>]`.
        #[arg(long, value_name = "URL")]
        ledger: Option<String>,
        /// Front door this store is reached through: `https://<host>[:<port>]`,
        /// or `http://` to a loopback literal.
        ///
        /// Spelled `--front-door` and not `--server` because the global
        /// `--server` already owns that long name in every subcommand, and clap
        /// refuses a duplicate outright. The collision is a mercy: the two mean
        /// different things. The global routes THIS invocation through a front
        /// door; this one records the front door the store is reached through.
        #[arg(long, value_name = "URL")]
        front_door: Option<String>,
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
        /// Clear only the front door. Spelled to match `set --front-door`.
        #[arg(long)]
        front_door: bool,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server_reachable: Option<Reach>,
}

impl Checked {
    fn line(label: &str, r: Option<&Reach>) -> Option<String> {
        match r? {
            Reach::Ok => Some(format!("{label}: reachable")),
            Reach::Fault(why) => Some(format!("{label}: UNUSABLE — {why}")),
        }
    }
    fn lines(&self) -> Vec<String> {
        [
            Self::line("ledger", self.ledger_reachable.as_ref()),
            Self::line("server", self.server_reachable.as_ref()),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct RemoteSet {
    ledger: Option<String>,
    server: Option<String>,
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
            // Looked up per key rather than printing `ledger` for every row:
            // this used to be a single-url command, and echoing the wrong url
            // back at an operator who just set two is how a typo survives.
            let url = match k.as_str() {
                "server" => self.server.as_deref(),
                _ => self.ledger.as_deref(),
            };
            out.push(format!("{k} set to {}", url.unwrap_or("")));
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
    server: Option<String>,
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
            out.push("local-only — no ledger, no server".to_string());
            return out.join("\n");
        }
        if let Some(u) = &self.ledger {
            out.push(format!("ledger:    {u}"));
        }
        if let Some(u) = &self.server {
            out.push(format!("server:    {u}"));
        }
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
    server: Option<String>,
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
            front_door,
            check,
            json,
        } => {
            if ledger.is_none() && front_door.is_none() {
                return Err(db::invalid_input(
                    "nothing to set: pass --ledger <url> or --front-door <url>",
                ));
            }
            // Each url is validated by its OWN shape check, and both are
            // written before anything is reported: one command setting two
            // urls must not leave the first written and the second refused.
            if let Some(url) = &ledger {
                remote::validate_ledger_url(url)?;
            }
            if let Some(url) = &front_door {
                remote::validate_server_url(url)?;
            }
            let mut changed: Vec<&'static str> = Vec::new();
            if let Some(url) = &ledger {
                remote::set(&conn, remote::KEY_LEDGER, url)?;
                changed.push("ledger");
            }
            if let Some(url) = &front_door {
                remote::set(&conn, remote::KEY_SERVER, url)?;
                changed.push("server");
            }
            let now = remote::load(&conn)?;
            let checked = probe_all(&now, check, &changed);
            emit(
                json,
                &RemoteSet {
                    ledger: now.ledger,
                    server: now.server,
                    changed: changed.into_iter().map(str::to_string).collect(),
                    checked,
                },
            )
        }
        RemoteOp::Show { check, json } => {
            let now = remote::load(&conn)?;
            let checked = probe_all(&now, check, &["ledger", "server"]);
            emit(
                json,
                &RemoteShow {
                    project_uuid: crate::project::store_uuid(&conn)?,
                    project_key: crate::project::require(&conn)?,
                    local_only: now.local_only(),
                    ledger: now.ledger,
                    server: now.server,
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
        RemoteOp::Clear {
            ledger,
            front_door,
            all,
            json,
        } => {
            // No flag still means everything, which is what `clear` has always
            // meant and what a script that already says `qp remote clear`
            // depends on. `--all` is the explicit spelling of the same thing.
            let every = all || (!ledger && !front_door);
            let mut cleared: Vec<&'static str> = Vec::new();
            if (every || ledger) && remote::clear(&conn, remote::KEY_LEDGER)? {
                cleared.push("ledger");
            }
            if (every || front_door) && remote::clear(&conn, remote::KEY_SERVER)? {
                cleared.push("server");
            }
            let now = remote::load(&conn)?;
            emit(
                json,
                &RemoteCleared {
                    cleared: cleared.into_iter().map(str::to_string).collect(),
                    local_only: now.local_only(),
                    ledger: now.ledger,
                    server: now.server,
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
            server_reachable: None,
        };
    }
    let one = |key: &str, url: &Option<String>, probe: fn(&str) -> Reach| -> Option<Reach> {
        if !which.contains(&key) {
            return None;
        }
        url.as_deref().map(probe)
    };
    Checked {
        ledger_reachable: one("ledger", &r.ledger, remote::probe),
        server_reachable: one("server", &r.server, |u| remote::probe_server(u).reach()),
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
            ledger_reachable: Some(Reach::Fault("bucket not found".to_string())),
            server_reachable: Some(Reach::Ok),
        };
        crate::outcome::round_trip::assert_round_trips(&RemoteSet {
            ledger: Some("s3://example/ledger".to_string()),
            server: Some("https://qp.example.com".to_string()),
            changed: vec!["ledger".to_string(), "server".to_string()],
            checked: checked(),
        });
        crate::outcome::round_trip::assert_round_trips(&RemoteShow {
            project_uuid: "00000000-0000-4000-8000-000000000000".to_string(),
            project_key: "example".to_string(),
            ledger: Some("s3://example/ledger".to_string()),
            server: Some("https://qp.example.com".to_string()),
            local_only: false,
            checked: checked(),
        });
    }
}
