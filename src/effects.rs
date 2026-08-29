//! Envelope-level report of tasks a transaction moved that the caller never named.
//!
#![doc = include_str!("../docs/modules/effects.md")]

use anyhow::Result;
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};

/// One state change the caller did not ask for, in the caller's own vocabulary.
///
/// `task` is the canonical `display_id`, never the rowid the sweeps deal in —
/// a rowid names a row in the store that minted it and means something else,
/// or nothing, anywhere the payload might travel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub struct Effect {
    /// Always `state_change` today. Present so a later effect kind is additive
    /// rather than a second, differently-shaped list.
    pub kind: String,
    pub task: String,
    pub from: String,
    pub to: String,
    /// The same `via` the `state_change` event carries, so the report and the
    /// log name the cause identically.
    pub via: String,
}

/// Resolves the sweeps' raw rowids into the `_effects` list for a payload.
///
/// `named` is every task the request itself mentioned; those are dropped. A
/// caller that names a task is told about it by the rest of the payload, and
/// `_effects` exists for the ones nothing else reports.
///
/// This is the single resolution point on purpose. Eight verbs move tasks they
/// were not asked about, and every one of them held a `Vec<i64>` and threw it
/// away; a per-verb `SELECT display_id` in each would be eight chances to emit
/// a rowid onto the wire.
pub fn collateral(
    tx: &Transaction,
    named: &[i64],
    via: &str,
    m: &crate::db::Movements,
) -> Result<Vec<Effect>> {
    let mut out = Vec::new();
    for (ids, from, to) in [
        (
            &m.demoted,
            crate::db::State::Ready,
            crate::db::State::Pending,
        ),
        (
            &m.promoted,
            crate::db::State::Pending,
            crate::db::State::Ready,
        ),
    ] {
        let mut ids: Vec<i64> = ids.iter().copied().filter(|i| !named.contains(i)).collect();
        ids.sort_unstable();
        for id in ids {
            out.push(Effect {
                kind: "state_change".to_string(),
                task: display_id(tx, id)?,
                from: from.as_str().to_string(),
                to: to.as_str().to_string(),
                via: via.to_string(),
            });
        }
    }
    Ok(out)
}

/// The store's canonical name for a rowid.
fn display_id(tx: &Transaction, id: i64) -> Result<String> {
    Ok(
        tx.query_row("SELECT display_id FROM task WHERE id = ?", [id], |r| {
            r.get(0)
        })?,
    )
}
