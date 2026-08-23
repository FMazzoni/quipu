//! Attach a store to a claim authority and a ledger, or detach it back to local-only.
//!
#![doc = include_str!("../../docs/modules/remote.md")]

use crate::db;
use crate::outcome::{emit, Outcome};
use crate::remote::{self, Reach, Remotes};
use anyhow::Result;
use clap::{Args, Subcommand};
use serde::Serialize;

#[derive(Args, Debug)]
pub struct RemoteArgs {
    #[command(subcommand)]
    pub op: RemoteOp,
}

#[derive(Subcommand, Debug)]
pub enum RemoteOp {
    /// Point this store at a claim authority and/or a ledger (config only)
    Set {
        /// Where claims are refereed. Reachability matters; ticket content never crosses it.
        #[arg(long, value_name = "URL")]
        authority: Option<String>,
        /// Where the ledger syncs. Content crosses it; only your own machines need reach it.
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
    /// Detach — the deliberate route back to local-only operation
    Clear {
        /// Clear only the authority.
        #[arg(long)]
        authority: bool,
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
            | RemoteOp::Clear { json, .. } => *json,
        }
    }
}

/// Reachability as it appears in output: absent unless `--check` asked for it.
#[derive(Serialize)]
struct Checked {
    /// Field names carry the `_reachable` suffix because `Checked` is
    /// `#[serde(flatten)]`ed next to the url fields; bare `authority` would
    /// emit the key twice.
    #[serde(skip_serializing_if = "Option::is_none")]
    authority_reachable: Option<Reach>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ledger_reachable: Option<Reach>,
}

impl Checked {
    fn line(label: &str, r: Option<Reach>) -> Option<String> {
        match r? {
            Reach::Ok => Some(format!("{label}: reachable")),
            Reach::Unreachable => Some(format!("{label}: UNREACHABLE")),
            Reach::NoGit => Some(format!("{label}: could not run git")),
        }
    }
    fn lines(&self) -> Vec<String> {
        [
            Self::line("authority", self.authority_reachable),
            Self::line("ledger", self.ledger_reachable),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

#[derive(Serialize)]
struct RemoteSet {
    authority: Option<String>,
    ledger: Option<String>,
    /// Which keys this invocation wrote — `set --ledger` leaves the authority alone.
    changed: Vec<&'static str>,
    #[serde(flatten)]
    checked: Checked,
}
impl Outcome for RemoteSet {
    fn human(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        for k in &self.changed {
            let url = match *k {
                "authority" => self.authority.as_deref(),
                _ => self.ledger.as_deref(),
            };
            out.push(format!("{k} set to {}", url.unwrap_or("")));
        }
        out.extend(self.checked.lines());
        out.join("\n")
    }
}

#[derive(Serialize)]
struct RemoteShow {
    authority: Option<String>,
    ledger: Option<String>,
    /// True when neither url is set. The default state, and a supported one.
    local_only: bool,
    #[serde(flatten)]
    checked: Checked,
}
impl Outcome for RemoteShow {
    fn human(&self) -> String {
        if self.local_only {
            return "local-only — no authority, no ledger".to_string();
        }
        let mut out = vec![
            format!(
                "authority: {}",
                self.authority.as_deref().unwrap_or("(unset)")
            ),
            format!("ledger:    {}", self.ledger.as_deref().unwrap_or("(unset)")),
        ];
        out.extend(self.checked.lines());
        out.join("\n")
    }
}

#[derive(Serialize)]
struct RemoteCleared {
    /// Keys that actually held a value. Clearing an unset key is a no-op, not an error.
    cleared: Vec<&'static str>,
    authority: Option<String>,
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
            authority,
            ledger,
            check,
            json,
        } => {
            if authority.is_none() && ledger.is_none() {
                return Err(db::invalid_input(
                    "nothing to set: pass --authority <url>, --ledger <url>, or both",
                ));
            }
            let mut changed: Vec<&'static str> = Vec::new();
            if let Some(url) = &authority {
                remote::validate_url(url)?;
                remote::set(&conn, remote::KEY_AUTHORITY, url)?;
                changed.push("authority");
            }
            if let Some(url) = &ledger {
                remote::validate_url(url)?;
                remote::set(&conn, remote::KEY_LEDGER, url)?;
                changed.push("ledger");
            }
            let now = remote::load(&conn)?;
            let checked = probe_all(db_path, &now, check, &changed);
            emit(
                json,
                &RemoteSet {
                    authority: now.authority,
                    ledger: now.ledger,
                    changed,
                    checked,
                },
            )
        }
        RemoteOp::Show { check, json } => {
            let now = remote::load(&conn)?;
            let checked = probe_all(db_path, &now, check, &["authority", "ledger"]);
            emit(
                json,
                &RemoteShow {
                    local_only: now.local_only(),
                    authority: now.authority,
                    ledger: now.ledger,
                    checked,
                },
            )
        }
        RemoteOp::Clear {
            authority,
            ledger,
            all,
            json,
        } => {
            // No flag means both. `clear` is the escape hatch from an
            // unreachable authority, so the shortest thing to type is the one
            // that unblocks you.
            let (do_auth, do_ledger) = if all || (!authority && !ledger) {
                (true, true)
            } else {
                (authority, ledger)
            };
            let mut cleared: Vec<&'static str> = Vec::new();
            if do_auth && remote::clear(&conn, remote::KEY_AUTHORITY)? {
                cleared.push("authority");
            }
            if do_ledger && remote::clear(&conn, remote::KEY_LEDGER)? {
                cleared.push("ledger");
            }
            let now = remote::load(&conn)?;
            emit(
                json,
                &RemoteCleared {
                    cleared,
                    local_only: now.local_only(),
                    authority: now.authority,
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
fn probe_all(
    db_path: &std::path::Path,
    r: &Remotes,
    check: bool,
    which: &[&'static str],
) -> Checked {
    if !check {
        return Checked {
            authority_reachable: None,
            ledger_reachable: None,
        };
    }
    let lane = "remote-check";
    let one = |key: &str, url: &Option<String>| -> Option<Reach> {
        if !which.contains(&key) {
            return None;
        }
        url.as_deref().map(|u| remote::probe(db_path, u, lane))
    };
    Checked {
        authority_reachable: one("authority", &r.authority),
        ledger_reachable: one("ledger", &r.ledger),
    }
}
