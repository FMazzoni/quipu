//! `qp add` — create a task.
//!
#![doc = include_str!("../../docs/modules/add.md")]

use crate::outcome::{emit, Outcome};
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::Serialize;
use std::collections::HashSet;

#[derive(Args, Debug)]
pub struct AddArgs {
    pub title: String,
    #[arg(long)]
    pub tier: Option<String>,
    #[arg(long)]
    pub description: Option<String>,
    #[arg(long = "depends-on", value_name = "TASK_ID")]
    pub depends_on: Vec<String>,
    /// Container this task is part of. Note the direction: unlike
    /// `--depends-on`, the *named* task is the one that ends up waiting.
    #[arg(long = "part-of", value_name = "TASK_ID")]
    pub part_of: Vec<String>,
    #[arg(long, value_name = "NAME")]
    pub tag: Vec<String>,
    /// Required with `--part-of` when the container is assigned/running: the
    /// container is the row that gains an edge, so its owner has to be named.
    #[arg(long = "as")]
    pub agent: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct Created {
    display_id: String,
    title: String,
    state: String,
    tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    tags: Vec<String>,
    /// Containers this task was attached to, empty unless `--part-of` was given.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    part_of: Vec<String>,
}
impl Outcome for Created {
    fn human(&self) -> String {
        format!("{}\t{}\t{}", self.display_id, self.state, self.title)
    }
}

/// Creates a task, optionally wiring it into the graph in the same transaction.
///
/// # `--depends-on` and `--part-of` point opposite ways
///
/// Both write `dep` rows, and both are about this new task, but they put it on
/// opposite ends of the edge:
///
/// - `--depends-on X` — the new task waits for `X`. New task is the depender,
///   and starts `pending`.
/// - `--part-of X` — `X` waits for the new task, because the new task is one of
///   the things `X` is made of. `X` is the depender.
///
/// So `--part-of` puts the *named* task on the waiting end. Getting this
/// backwards produces a graph that looks plausible and rolls up exactly wrong,
/// which is why it is spelled out here and pinned by
/// `add_part_of_attaches_to_a_container` in `tests/cli.rs`.
///
/// The new task is not always unaffected: attaching it to a container that is
/// itself blocked puts it inside a frozen subtree, so it arrives `pending`.
pub fn run(db_path: &std::path::Path, a: AddArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    // Pre-resolve deps outside tx; errors early.
    let mut dep_ids = Vec::with_capacity(a.depends_on.len());
    for d in &a.depends_on {
        dep_ids.push(id::resolve(&conn, d)?);
    }
    let parents = a
        .part_of
        .iter()
        .map(|p| id::resolve_full(&conn, p))
        .collect::<Result<Vec<_>>>()?;

    let created = db::with_tx(&mut conn, |tx| {
        let state = if dep_ids.is_empty() {
            db::STATE_READY
        } else {
            db::STATE_PENDING
        };
        tx.execute(
            "INSERT INTO task(display_id, title, tier, description, state) VALUES ('', ?, ?, ?, ?)",
            rusqlite::params![a.title, a.tier, a.description, state],
        )?;
        let row = tx.last_insert_rowid();
        let prefix = db::display_prefix(tx)?;
        let display = id::encode(row, &prefix);
        tx.execute(
            "UPDATE task SET display_id = ? WHERE id = ?",
            rusqlite::params![display, row],
        )?;
        for did in &dep_ids {
            // Cycle check: dep edges only go from `row` outward, so cycle only possible if
            // some existing edge from *did to row exists. Use would_cycle for safety.
            if db::would_cycle(tx, row, *did)? {
                return Err(db::invariant(
                    "dependency_cycle",
                    format!(
                        "cycle: {} depends on dep#{} which (transitively) depends on {}",
                        display, did, display
                    ),
                ));
            }
            tx.execute(
                "INSERT INTO dep(task_id, depends_on_task_id, mode) VALUES (?,?,?)",
                rusqlite::params![row, did, db::MODE_BLOCKS],
            )?;
        }
        // Direction inverts here: the container is the depender. See the note on
        // `run`. Ownership is gated per parent — attaching to someone's running
        // wave is a mutation of their row.
        for parent in &parents {
            db::require_edge_owner(tx, parent.id, &parent.display_id, a.agent.as_deref())?;
            db::link_dep(
                tx,
                parent.id,
                &parent.display_id,
                row,
                &display,
                db::MODE_CONTAINS,
                a.agent.as_deref(),
            )?;
        }
        let defaults = db::default_tags(tx)?;
        let mut merged: HashSet<String> = HashSet::new();
        for t in defaults {
            merged.insert(t);
        }
        for t in &a.tag {
            merged.insert(t.clone());
        }
        let mut merged_tags: Vec<String> = merged.into_iter().collect();
        merged_tags.sort();
        for tag in &merged_tags {
            tx.execute(
                "INSERT OR IGNORE INTO tag(task_id, name) VALUES (?,?)",
                rusqlite::params![row, tag],
            )?;
        }
        // If deps were added, run refresh_ready now: if all deps are already done/cancelled
        // this task may immediately transition to ready.
        if !dep_ids.is_empty() {
            db::refresh_ready(tx)?;
        }
        let actual_state: String =
            tx.query_row("SELECT state FROM task WHERE id = ?", [row], |r| r.get(0))?;
        db::insert_event(
            tx,
            Some(row),
            "state_change",
            None,
            Some(&serde_json::json!({"to": actual_state, "title": a.title})),
        )?;
        Ok(Created {
            display_id: display,
            title: a.title.clone(),
            state: actual_state,
            tier: a.tier.clone(),
            description: a.description.clone(),
            tags: merged_tags,
            part_of: parents.iter().map(|p| p.display_id.clone()).collect(),
        })
    })?;

    emit(a.json, &created)
}
