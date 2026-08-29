//! Manage typed, non-blocking references between tasks.
//!
#![doc = include_str!("../../docs/modules/relation.md")]

use crate::outcome::{emit, Outcome};
use crate::{db, id};
use anyhow::Result;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Args, Debug)]
pub struct RelationArgs {
    #[command(subcommand)]
    pub op: RelOp,
}

#[derive(Subcommand, Debug)]
pub enum RelOp {
    Add {
        from: String,
        kind: String,
        to: String,
        #[arg(long)]
        json: bool,
    },
    Rm {
        from: String,
        kind: String,
        to: String,
        #[arg(long)]
        json: bool,
    },
    List(RelationListArgs),
}

/// `qp relation list <task>` — the one relation op the front door answers.
///
/// A struct rather than an inline variant because `route::Served` is bound by
/// `clap::Args`, which an enum variant is not. The CLI surface is unchanged.
#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationListArgs {
    pub task: String,
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
}

/// Whether this `relation` invocation is the served `list` op.
///
/// One `Cmd` variant carries all three ops and only this one goes over the
/// wire, so `is_served` needs the distinction: without it a `--server relation
/// add` would skip the attach check and then write the *local* store.
pub fn is_list(a: &RelationArgs) -> bool {
    matches!(a.op, RelOp::List(_))
}

impl crate::route::Served for RelationListArgs {
    /// Nested under the group it belongs to.
    ///
    /// A flattened name could collide with a future top-level `list`-shaped
    /// verb; this one cannot.
    const VERB: &'static str = "relation/list";
    type Out = RelationList;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec_list(db_path, a)
    }
}

/// The two directions of a task's relation edges, in one object.
///
/// One endpoint, not two: `incoming` and `outgoing` are the same question asked
/// from both ends and every caller wants both — `qp relation list QP-1` after a
/// `supersedes` link is exactly the audit trail that has to work from either
/// side.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct RelationList(Value);

impl Outcome for RelationList {
    fn human(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        for o in self.0["outgoing"].as_array().into_iter().flatten() {
            lines.push(format!(
                "→ {} {}",
                o["kind"].as_str().unwrap_or(""),
                o["to"].as_str().unwrap_or("")
            ));
        }
        for i in self.0["incoming"].as_array().into_iter().flatten() {
            lines.push(format!(
                "← {} {}",
                i["kind"].as_str().unwrap_or(""),
                i["from"].as_str().unwrap_or("")
            ));
        }
        lines.join("\n")
    }
}

fn exec_list(db_path: &std::path::Path, a: RelationListArgs) -> Result<RelationList> {
    let conn = db::open(db_path)?;
    let task_id = id::resolve(&conn, &a.task)?;
    let outgoing: Vec<Value> = {
        let mut s = conn.prepare(
            "SELECT t.display_id, r.kind FROM relation r
               JOIN task t ON t.id = r.to_task_id
              WHERE r.from_task_id = ? ORDER BY r.kind, t.id",
        )?;
        let x = s
            .query_map([task_id], |r| {
                Ok(serde_json::json!({
                    "to": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?
                }))
            })?
            .collect::<Result<_, _>>()?;
        x
    };
    let incoming: Vec<Value> = {
        let mut s = conn.prepare(
            "SELECT t.display_id, r.kind FROM relation r
               JOIN task t ON t.id = r.from_task_id
              WHERE r.to_task_id = ? ORDER BY r.kind, t.id",
        )?;
        let x = s
            .query_map([task_id], |r| {
                Ok(serde_json::json!({
                    "from": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?
                }))
            })?
            .collect::<Result<_, _>>()?;
        x
    };
    Ok(RelationList(
        serde_json::json!({"outgoing": outgoing, "incoming": incoming}),
    ))
}

impl RelationArgs {
    /// Whether `--json` was passed on whichever subcommand was chosen.
    ///
    /// Lets `main`'s error path match the stream format the success path uses.
    pub fn json(&self) -> bool {
        match &self.op {
            RelOp::Add { json, .. } | RelOp::Rm { json, .. } => *json,
            RelOp::List(a) => a.json,
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

pub fn run(db_path: &std::path::Path, a: RelationArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    match a.op {
        RelOp::Add {
            from,
            kind,
            to,
            json,
        } => {
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
            emit(
                json,
                &RelationAdded {
                    from_display_id: f.display_id,
                    to_display_id: t.display_id,
                    kind,
                    added,
                },
            )?;
        }
        RelOp::Rm {
            from,
            kind,
            to,
            json,
        } => {
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
            emit(
                json,
                &RelationRemoved {
                    from_display_id: f.display_id,
                    to_display_id: t.display_id,
                    kind,
                    removed,
                },
            )?;
        }
        // Served, and dispatched from `real_main` before this function is
        // reached. The arm stays so the match is exhaustive and so a reader
        // finding `relation list` here is sent to the one implementation.
        RelOp::List(a) => {
            let json = a.json;
            let out = exec_list(db_path, a)?;
            emit(json, &out)?;
        }
    }
    Ok(())
}
