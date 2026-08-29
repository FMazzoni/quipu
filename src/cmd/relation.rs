//! Manage typed, non-blocking references between tasks.
//!
#![doc = include_str!("../../docs/modules/relation.md")]

use crate::outcome::Outcome;
use crate::{db, id};
use anyhow::Result;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationArgs {
    #[command(subcommand)]
    pub op: RelOp,
}

/// The chosen op, externally tagged so the body reads `{"op":{"add":{...}}}`.
///
/// Lower-cased to match what the caller types. `--json` is skipped on every
/// variant for the same reason it is skipped on every other args struct: it is
/// a property of the invoking terminal and never crosses the socket.
#[derive(Subcommand, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum RelOp {
    Add {
        from: String,
        kind: String,
        to: String,
        #[arg(long)]
        #[serde(skip)]
        json: bool,
    },
    Rm {
        from: String,
        kind: String,
        to: String,
        #[arg(long)]
        #[serde(skip)]
        json: bool,
    },
    List {
        task: String,
        #[arg(long)]
        #[serde(skip)]
        json: bool,
    },
}

impl crate::route::Served for RelationArgs {
    const VERB: &'static str = "relation";
    type Out = RelationOutcome;
    fn wants_json(&self) -> bool {
        self.json()
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

impl RelationArgs {
    /// Whether `--json` was passed on whichever subcommand was chosen.
    ///
    /// Lets `main`'s error path match the stream format the success path uses.
    pub fn json(&self) -> bool {
        match &self.op {
            RelOp::Add { json, .. } | RelOp::Rm { json, .. } | RelOp::List { json, .. } => *json,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct RelationAdded {
    from_display_id: String,
    to_display_id: String,
    kind: String,
    added: bool,
}
impl Outcome for RelationAdded {
    fn human(&self) -> String {
        if self.added {
            format!(
                "{} {} {} linked",
                self.from_display_id, self.kind, self.to_display_id
            )
        } else {
            format!(
                "{} {} {} already linked",
                self.from_display_id, self.kind, self.to_display_id
            )
        }
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct RelationRemoved {
    from_display_id: String,
    to_display_id: String,
    kind: String,
    removed: bool,
}
impl Outcome for RelationRemoved {
    fn human(&self) -> String {
        if self.removed {
            format!(
                "{} {} {} unlinked",
                self.from_display_id, self.kind, self.to_display_id
            )
        } else {
            format!(
                "{} {} {} not linked",
                self.from_display_id, self.kind, self.to_display_id
            )
        }
    }
}

/// One route, three ops — because a `Served` impl is per args-struct.
///
/// `relation` is a subcommand group, and the group is what the CLI dispatches
/// on: `route::is_served` matches `Cmd::Relation(_)`, so the group is served
/// whole or not at all. Serving only `add`/`rm` would leave `relation list`
/// running against whatever store happens to be on the *client's* disk — which,
/// for a caller pointed at a front door, is usually no store at all. That is
/// the failure the wire contract keeps `remote`'s subtree whole to avoid,
/// arrived at from the other side.
///
/// `untagged`, so each variant is on the wire as the bare object it already
/// was. An externally-tagged enum would wrap every payload in `{"Added": ...}`
/// and break every existing `--json` consumer for nothing. The three shapes are
/// distinguishable by their required fields, which is what makes `untagged`
/// safe here rather than merely convenient.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum RelationOutcome {
    Added(RelationAdded),
    Removed(RelationRemoved),
    Listed(RelationListed),
}

/// Hand-written because `#[default]` only takes unit variants: the registry in
/// `outcome.rs` needs a sample of every payload, and this one wraps a struct.
#[cfg(test)]
impl Default for RelationOutcome {
    fn default() -> Self {
        RelationOutcome::Added(RelationAdded::default())
    }
}

impl Outcome for RelationOutcome {
    fn human(&self) -> String {
        match self {
            RelationOutcome::Added(o) => o.human(),
            RelationOutcome::Removed(o) => o.human(),
            RelationOutcome::Listed(o) => o.human(),
        }
    }
}

/// Both directions of one task's relation edges.
#[derive(Serialize, Deserialize, Default)]
pub(crate) struct RelationListed {
    outgoing: Vec<Outgoing>,
    incoming: Vec<Incoming>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Outgoing {
    to: String,
    kind: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Incoming {
    from: String,
    kind: String,
}

impl RelationListed {
    /// The arrows the CLI has always printed, as one block.
    ///
    /// Not an `impl Outcome`: the registry in `outcome.rs` is the set of
    /// payloads that cross the wire on their own, and this one only ever
    /// travels inside [`RelationOutcome`].
    fn human(&self) -> String {
        let mut lines = Vec::new();
        for o in &self.outgoing {
            lines.push(format!("→ {} {}", o.kind, o.to));
        }
        for i in &self.incoming {
            lines.push(format!("← {} {}", i.kind, i.from));
        }
        lines.join("\n")
    }
}

pub fn exec(db_path: &std::path::Path, a: RelationArgs) -> Result<RelationOutcome> {
    let mut conn = db::open(db_path)?;
    match a.op {
        RelOp::Add { from, kind, to, .. } => {
            if kind.is_empty() {
                return Err(db::invalid_input("relation kind required"));
            }
            let f = id::resolve_full(&conn, &from)?;
            let t = id::resolve_full(&conn, &to)?;
            let added = db::with_tx(&mut conn, |tx| -> Result<bool> {
                tx.execute(
                    "INSERT OR IGNORE INTO relation(from_task_id, to_task_id, kind) VALUES (?,?,?)",
                    rusqlite::params![f.id, t.id, kind],
                )?;
                if tx.changes() == 1 {
                    db::insert_event(
                        tx,
                        Some(f.id),
                        "relation_add",
                        None,
                        Some(&serde_json::json!({"kind": kind, "to": t.display_id})),
                    )?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            })?;
            Ok(RelationOutcome::Added(RelationAdded {
                from_display_id: f.display_id,
                to_display_id: t.display_id,
                kind,
                added,
            }))
        }
        RelOp::Rm { from, kind, to, .. } => {
            let f = id::resolve_full(&conn, &from)?;
            let t = id::resolve_full(&conn, &to)?;
            let removed = db::with_tx(&mut conn, |tx| -> Result<bool> {
                tx.execute(
                    "DELETE FROM relation WHERE from_task_id = ? AND to_task_id = ? AND kind = ?",
                    rusqlite::params![f.id, t.id, kind],
                )?;
                if tx.changes() > 0 {
                    db::insert_event(
                        tx,
                        Some(f.id),
                        "relation_removed",
                        None,
                        Some(&serde_json::json!({"kind": kind, "to": t.display_id})),
                    )?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            })?;
            Ok(RelationOutcome::Removed(RelationRemoved {
                from_display_id: f.display_id,
                to_display_id: t.display_id,
                kind,
                removed,
            }))
        }
        RelOp::List { task, .. } => {
            let task_id = id::resolve(&conn, &task)?;
            let outgoing: Vec<Outgoing> = {
                let mut s = conn.prepare(
                    "SELECT t.display_id, r.kind FROM relation r
                       JOIN task t ON t.id = r.to_task_id
                      WHERE r.from_task_id = ? ORDER BY r.kind, t.id",
                )?;
                let x = s
                    .query_map([task_id], |r| {
                        Ok(Outgoing {
                            to: r.get(0)?,
                            kind: r.get(1)?,
                        })
                    })?
                    .collect::<Result<_, _>>()?;
                x
            };
            let incoming: Vec<Incoming> = {
                let mut s = conn.prepare(
                    "SELECT t.display_id, r.kind FROM relation r
                       JOIN task t ON t.id = r.from_task_id
                      WHERE r.to_task_id = ? ORDER BY r.kind, t.id",
                )?;
                let x = s
                    .query_map([task_id], |r| {
                        Ok(Incoming {
                            from: r.get(0)?,
                            kind: r.get(1)?,
                        })
                    })?
                    .collect::<Result<_, _>>()?;
                x
            };
            Ok(RelationOutcome::Listed(RelationListed {
                outgoing,
                incoming,
            }))
        }
    }
}
