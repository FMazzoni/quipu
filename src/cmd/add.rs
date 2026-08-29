//! `qp add` — create a task.
//!
#![doc = include_str!("../../docs/modules/add.md")]

use crate::effects::{self, Effect};
use crate::outcome::Outcome;
use crate::{db, id};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddArgs {
    pub title: String,
    #[arg(long)]
    #[serde(default)]
    pub tier: Option<String>,
    #[arg(long)]
    #[serde(default)]
    pub description: Option<String>,
    #[arg(long = "depends-on", value_name = "TASK_ID")]
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Container this task is part of. Note the direction: unlike
    /// `--depends-on`, the *named* task is the one that ends up waiting.
    #[arg(long = "part-of", value_name = "TASK_ID")]
    #[serde(default)]
    pub part_of: Vec<String>,
    #[arg(long, value_name = "NAME")]
    #[serde(default)]
    pub tag: Vec<String>,
    /// Required with `--part-of` when the container is assigned/running: the
    /// container is the row that gains an edge, so its owner has to be named.
    #[arg(long = "as")]
    #[serde(default)]
    pub agent: Option<String>,
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
}

impl crate::route::Served for AddArgs {
    const VERB: &'static str = "add";
    type Out = Created;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct Created {
    display_id: String,
    title: String,
    state: String,
    tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    tags: Vec<String>,
    /// Containers this task was attached to, empty unless `--part-of` was given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    part_of: Vec<String>,
    /// Tasks this transaction moved that the caller did not name.
    ///
    /// A create can move existing work in both directions: joining a container
    /// whose deps are resolved promotes nothing, but joining a blocked one
    /// freezes its siblings' subtree, and naming a `--depends-on` can free it.
    #[serde(rename = "_effects", default, skip_serializing_if = "Vec::is_empty")]
    effects: Vec<Effect>,
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
pub fn exec(db_path: &std::path::Path, a: AddArgs) -> Result<Created> {
    let mut conn = db::open(db_path)?;
    // Pre-resolve deps outside tx; errors early.
    let mut dep_ids = Vec::with_capacity(a.depends_on.len());
    for d in &a.depends_on {
        dep_ids.push(id::resolve_full(&conn, d)?);
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
        // The creation event leads, before any edge or tag that references this
        // task. A log whose first mention of a task is an edge pointing at it
        // cannot be replayed in order, and everything below this line can move
        // the row's state, so this is also the only point at which the state
        // written here is the state the row was born in.
        //
        // `from` is `null` rather than absent: a task has no prior state, and
        // that is a different claim from an event too old to record one.
        // `tier` and `description` ride along because the `INSERT` above is the
        // only place they are ever set without an `edit` event to say so.
        let mut birth: Vec<(&str, serde_json::Value)> = vec![("title", serde_json::json!(a.title))];
        if let Some(t) = &a.tier {
            birth.push(("tier", serde_json::json!(t)));
        }
        if let Some(d) = &a.description {
            birth.push(("description", serde_json::json!(d)));
        }
        db::insert_state_change(tx, row, None, state, None, &birth)?;
        // Through the shared edge writer rather than a raw `INSERT`: the raw
        // insert wrote edges no `dep_added` event ever described, which is most
        // of how the dep graph became unreplayable. It also brings the cycle
        // check, vacuous for a task nothing points at yet and cheap enough to
        // keep for free rather than special-case.
        //
        // `write_dep_edge`, not `link_dep`, because a create can name several
        // edges and `link_dep` sweeps after each one. Sweeping between edges
        // reads a half-written graph: with `--depends-on DONE --depends-on OPEN`
        // the first sweep sees only the resolved dep, promotes the new task to
        // `ready`, and the second edge immediately demotes it again — logging a
        // `ready` the store was never in, and putting a `to: "ready"` on the
        // wire for anything watching. All the edges go in first; the single
        // sweep below then runs once, against the finished graph.
        for dep in &dep_ids {
            db::write_dep_edge(
                tx,
                row,
                &display,
                dep.id,
                &dep.display_id,
                db::MODE_BLOCKS,
                a.agent.as_deref(),
            )?;
        }
        // Direction inverts here: the container is the depender. See the note on
        // `run`. Ownership is gated per parent — attaching to someone's running
        // wave is a mutation of their row.
        for parent in &parents {
            db::require_edge_owner(tx, parent.id, &parent.display_id, a.agent.as_deref())?;
            db::write_dep_edge(
                tx,
                parent.id,
                &parent.display_id,
                row,
                &display,
                db::MODE_CONTAINS,
                a.agent.as_deref(),
            )?;
        }
        // The one sweep, after every edge is written. It promotes the new task
        // when all its deps were already resolved, and freezes it when a
        // container it just joined is itself blocked.
        let moved = db::reconcile_readiness(tx, a.agent.as_deref(), "add")?;
        // Everything the request itself mentions: the new row, its
        // `--depends-on` targets, and the containers it joined.
        let named: Vec<i64> = std::iter::once(row)
            .chain(dep_ids.iter().map(|d| d.id))
            .chain(parents.iter().map(|p| p.id))
            .collect();
        let effects = effects::collateral(tx, &named, "add", &moved)?;
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
        // One `tag_added` per tag that actually landed, same kind and payload
        // `qp tag add` writes. `--tag` is how essentially every tag in a store
        // arrives, so a silent insert here meant the log knew about almost none
        // of them. Guarded on `changes()` so a tag already present — only
        // possible via the default-tag merge — is not announced twice.
        for tag in &merged_tags {
            tx.execute(
                "INSERT OR IGNORE INTO tag(task_id, name) VALUES (?,?)",
                rusqlite::params![row, tag],
            )?;
            if tx.changes() == 1 {
                db::insert_event(
                    tx,
                    Some(row),
                    "tag_added",
                    a.agent.as_deref(),
                    Some(&serde_json::json!({"name": tag})),
                )?;
            }
        }
        // Read back for the *reported* state only. It can differ from the state
        // the row was born in, in both directions: deps already resolved
        // promote it, and a container that is itself blocked freezes it.
        let actual_state: String =
            tx.query_row("SELECT state FROM task WHERE id = ?", [row], |r| r.get(0))?;
        Ok(Created {
            display_id: display,
            title: a.title.clone(),
            state: actual_state,
            tier: a.tier.clone(),
            description: a.description.clone(),
            tags: merged_tags,
            part_of: parents.iter().map(|p| p.display_id.clone()).collect(),
            effects,
        })
    })?;

    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both `skip_serializing_if` fields populated.
    ///
    /// The registry's `Default` sample omits them; the two shapes fail in
    /// opposite directions.
    #[test]
    fn a_fully_populated_create_round_trips() {
        crate::outcome::round_trip::assert_round_trips(&Created {
            display_id: "QP-7".to_string(),
            title: "wire the payloads".to_string(),
            state: "pending".to_string(),
            tier: Some("slice".to_string()),
            description: Some("two tickets, one sweep".to_string()),
            tags: vec!["kind:chore".to_string()],
            part_of: vec!["QP-1".to_string()],
            effects: vec![Effect {
                kind: "state_change".to_string(),
                task: "QP-9".to_string(),
                from: "ready".to_string(),
                to: "pending".to_string(),
                via: "add".to_string(),
            }],
        });
    }
}
