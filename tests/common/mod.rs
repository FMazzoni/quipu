//! Shared harness for the two properties that hold the event log to account.
//!
//! `replay_equivalence` asserts the log rebuilds the projection; `export_roundtrip`
//! asserts the log survives a trip through the exported ledger and back. They
//! need the same three things — a wide mutation script, a replayer, and a
//! projection diff — so those live here rather than in one of them.
//!
//! Split out rather than duplicated on purpose: two copies of a replayer drift,
//! and the copy that drifts is the one whose property then proves nothing. The
//! reasoning behind each piece is in `replay_equivalence`'s module header, which
//! is where it was written.
//!
//! `mod common;` from an integration test compiles only the items that test uses,
//! so the unused-item warnings are suppressed wholesale below.

#![allow(dead_code)]

pub mod objstore;

use assert_cmd::Command;
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::Path;
/// The prefix both stores are initialised with. Display ids are derived from
/// it during replay, so it has to be the same on both sides.
pub const PREFIX: &str = "QP";

/// Applied to every `qp add` in the source store, which is what puts the
/// default-tag merge branch of `add` under test alongside explicit `--tag`.
pub const DEFAULT_TAG: &str = "proj:quipu";
// ---------------------------------------------------------------------------
// the mutation script
// ---------------------------------------------------------------------------

/// A wide sweep of mutations, ordered so the interesting transitions are the
/// ones nothing in the script names directly.
///
/// Display ids are literals rather than parsed back out of `--json` because the
/// sequence is the readable part: `QP-8` is minted by `qp block` at step 25 and
/// nowhere else, and a miscount fails loudly at the command that miscounted.
pub fn run_mutation_script(db: &Path) {
    // --- creation, covering every field `add` can write -------------------
    ok(
        db,
        &[
            "add",
            "root wave",
            "--tier",
            "wave",
            "--description",
            "the containing wave",
            "--tag",
            "kind:wave",
        ],
    ); // QP-1
    ok(
        db,
        &[
            "add",
            "slice a",
            "--part-of",
            "QP-1",
            "--tag",
            "kind:slice",
            "--tag",
            "prio:high",
        ],
    ); // QP-2, demotes QP-1
    ok(
        db,
        &[
            "add",
            "slice b",
            "--part-of",
            "QP-1",
            "--tier",
            "slice",
            "--description",
            "second slice",
        ],
    ); // QP-3
    ok(db, &["add", "prereq"]); // QP-4
    ok(
        db,
        &[
            "add",
            "dependent",
            "--depends-on",
            "QP-4",
            "--tag",
            "kind:dep",
            "--description",
            "waits on prereq",
        ],
    ); // QP-5
    ok(db, &["add", "wave blocker"]); // QP-6
    ok(db, &["add", "extra slice", "--tag", "kind:slice"]); // QP-7

    // --- graph and metadata written by the standalone commands ------------
    ok(db, &["contains", "QP-1", "QP-7"]);
    ok(db, &["relation", "add", "QP-5", "related-to", "QP-4"]);
    ok(db, &["relation", "add", "QP-2", "variant-of", "QP-3"]);
    ok(db, &["tag", "QP-4", "--as", "orch", "add", "urgent"]);
    ok(db, &["tag", "QP-2", "--as", "orch", "rm", "prio:high"]);
    ok(
        db,
        &[
            "edit",
            "QP-4",
            "--description",
            "prereq body",
            "--tier",
            "tier1",
        ],
    );
    ok(db, &["edit", "QP-1", "--title", "root wave (renamed)"]);
    ok(db, &["log", "QP-1", "note", "coordination note"]);

    // Direct demotion: the edge writer's own guarded ready -> pending.
    ok(db, &["depends", "QP-7", "--on", "QP-6"]);

    // INDIRECT DEMOTION: a blocker on the *container* freezes its contents.
    // QP-2 and QP-3 are `ready` and are named by nothing in this command.
    ok(db, &["depends", "QP-1", "--on", "QP-6"]);

    // INDIRECT PROMOTION: finishing that blocker thaws them again, two levels
    // down, plus QP-7's direct dep.
    ok(db, &["assign", "QP-6", "--to", "agentA"]);
    ok(db, &["claim", "QP-6", "--as", "agentA"]);
    ok(
        db,
        &[
            "complete",
            "QP-6",
            "--as",
            "agentA",
            "--decision",
            "cleared the wave",
            "--artifact",
            "notes.md",
        ],
    );

    // INDIRECT PROMOTION through a plain dep edge.
    ok(db, &["assign", "QP-4", "--to", "agentA"]);
    ok(db, &["claim", "QP-4", "--as", "agentA"]);
    ok(db, &["complete", "QP-4", "--as", "agentA"]);

    // `qp block`: mints QP-8 as a side effect and demotes the blocked task.
    ok(db, &["assign", "QP-3", "--to", "agentB"]);
    ok(db, &["claim", "QP-3", "--as", "agentB"]);
    ok(
        db,
        &[
            "block",
            "QP-3",
            "--as",
            "agentB",
            "--new",
            "unexpected blocker",
            "--tag",
            "kind:bug",
        ],
    ); // QP-8

    // Agent self-release, then orchestrator force-release from `assigned`.
    ok(db, &["assign", "QP-2", "--to", "agentC"]);
    ok(db, &["claim", "QP-2", "--as", "agentC"]);
    ok(
        db,
        &[
            "abandon",
            "QP-2",
            "--as",
            "agentC",
            "--reason",
            "wrong slice",
        ],
    );
    ok(db, &["assign", "QP-2", "--to", "agentD"]);
    ok(db, &["reclaim", "QP-2", "--reason", "stalled"]);

    // Cancel while an assignment is open, so `outcome = 'cancelled'` is covered.
    ok(db, &["assign", "QP-5", "--to", "agentE"]);
    ok(db, &["cancel", "QP-5", "--reason", "obsolete"]);

    // --- edge and metadata removal ----------------------------------------
    ok(db, &["depends", "QP-1", "--on", "QP-6", "--rm"]);
    ok(db, &["depends", "QP-7", "--on", "QP-6", "--rm"]);
    ok(db, &["contains", "QP-1", "QP-7", "--rm"]);
    ok(db, &["relation", "rm", "QP-2", "variant-of", "QP-3"]);
    ok(db, &["tag", "QP-2", "add", "prio:high"]); // re-add what was removed
                                                  // Clearing a field back to NULL, which `edit` spells as the empty string.
    ok(db, &["edit", "QP-3", "--tier", "", "--description", ""]);

    // INDIRECT PROMOTION via cancel: `cancelled` counts as resolved, so
    // cancelling QP-3's blocker brings QP-3 back without naming it.
    ok(db, &["cancel", "QP-8", "--reason", "not a real blocker"]);

    // --- drain, which promotes the container once its contents resolve ----
    ok(db, &["assign", "QP-2", "--to", "agentF"]);
    ok(db, &["claim", "QP-2", "--as", "agentF"]);
    ok(db, &["complete", "QP-2", "--as", "agentF"]);
    ok(db, &["assign", "QP-3", "--to", "agentF"]);
    ok(db, &["claim", "QP-3", "--as", "agentF"]);
    ok(db, &["complete", "QP-3", "--as", "agentF"]); // INDIRECT: promotes QP-1
    ok(db, &["assign", "QP-1", "--to", "agentG"]);
    ok(db, &["claim", "QP-1", "--as", "agentG"]);
    ok(db, &["complete", "QP-1", "--as", "agentG"]);
    // QP-7 is left `ready` on purpose: a store whose every task is terminal
    // would not diff the non-terminal states at all.
}
// ---------------------------------------------------------------------------
// the replayer
// ---------------------------------------------------------------------------

/// One row of the source `event` table, as replay sees it.
#[derive(Clone)]
pub struct Ev {
    pub id: i64,
    pub task_id: Option<i64>,
    pub ts: String,
    pub kind: String,
    pub agent_id: Option<String>,
    pub payload: serde_json::Value,
}

/// Rebuilds `dst`'s projection from `src`'s event log, and nothing else.
///
/// `keep` decides which events are applied; the property test keeps all of
/// them, and `harness_detects_a_lost_projection` drops a kind to prove the diff
/// notices. Returns the events it could not apply, described well enough to act
/// on — a missing transition is reported here rather than panicking, so a
/// failure shows the whole picture instead of the first symptom.
pub fn replay(src: &Path, dst: &Path, keep: impl Fn(&Ev) -> bool) -> Vec<String> {
    project(dst, &read_events(src), keep)
}

/// Reads a store's whole event log in id order.
pub fn read_events(src: &Path) -> Vec<Ev> {
    let s = Connection::open(src).unwrap();
    let mut stmt = s
        .prepare("SELECT id, task_id, ts, kind, agent_id, payload FROM event ORDER BY id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            let raw: Option<String> = r.get(5)?;
            Ok(Ev {
                id: r.get(0)?,
                task_id: r.get(1)?,
                ts: r.get(2)?,
                kind: r.get(3)?,
                agent_id: r.get(4)?,
                payload: raw
                    .map(|p| serde_json::from_str(&p).unwrap())
                    .unwrap_or(serde_json::Value::Null),
            })
        })
        .unwrap();
    rows.map(Result::unwrap).collect()
}

/// Projects a sequence of events onto an empty store, in the order given.
///
/// Taking the events as a slice rather than reading them itself is what lets
/// `export_roundtrip` feed in a log that arrived through the ledger files: the
/// projector cannot tell, and must not be able to tell, where they came from.
pub fn project(dst: &Path, events: &[Ev], keep: impl Fn(&Ev) -> bool) -> Vec<String> {
    let mut d = Connection::open(dst).unwrap();

    // The store-scoped seed. See the module header: `meta` has no events, so
    // the prefix cannot come from the log.
    let prefix: String = d
        .query_row(
            "SELECT value FROM meta WHERE key = 'display_prefix'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    let mut anomalies = Vec::new();
    let tx = d.transaction().unwrap();
    for ev in events {
        if !keep(ev) {
            continue;
        }
        apply(&tx, &prefix, ev, &mut anomalies);
    }
    tx.commit().unwrap();
    anomalies
}

/// Projects one event onto the replay store.
///
/// The catch-all arm is load-bearing: an event kind this function has never
/// heard of is an unreviewed claim about the projection, so it is reported
/// rather than ignored. That is what makes a *new* command's events fail the
/// property instead of quietly passing it.
pub fn apply(tx: &rusqlite::Transaction, prefix: &str, ev: &Ev, out: &mut Vec<String>) {
    let Some(task_id) = ev.task_id else {
        out.push(format!(
            "event {}: kind `{}` has a null task_id (store-scoped). The replayer \
             does not project store-scoped events — see the module header.",
            ev.id, ev.kind
        ));
        return;
    };
    // A task must exist before anything can be said about it. Without this the
    // failure surfaces as a bare foreign-key violation from deep inside the
    // replay, which names nothing and helps no one; the whole point of the
    // report is to say *which* task the log never created.
    let is_creation = ev.kind == "state_change"
        && ev
            .payload
            .get("from")
            .is_some_and(serde_json::Value::is_null);
    if !is_creation && !task_exists(tx, task_id) {
        out.push(format!(
            "event {}: kind `{}` names task {prefix}-{task_id}, which the log \
             never created. Some write minted a task without a creation event.",
            ev.id, ev.kind
        ));
        return;
    }

    match ev.kind.as_str() {
        "state_change" => apply_state_change(tx, prefix, ev, task_id, out),
        "tag_added" => {
            tx.execute(
                "INSERT OR IGNORE INTO tag(task_id, name) VALUES (?,?)",
                rusqlite::params![task_id, str_of(&ev.payload, "name")],
            )
            .unwrap();
        }
        "tag_removed" => {
            tx.execute(
                "DELETE FROM tag WHERE task_id = ? AND name = ?",
                rusqlite::params![task_id, str_of(&ev.payload, "name")],
            )
            .unwrap();
        }
        "dep_added" => {
            let on = rowid_of(&str_of(&ev.payload, "on"));
            let mode = str_of(&ev.payload, "mode");
            if !task_exists(tx, on) {
                out.push(format!(
                    "event {}: dep edge points at {prefix}-{on}, which the log \
                     never created",
                    ev.id
                ));
                return;
            }
            tx.execute(
                "INSERT INTO dep(task_id, depends_on_task_id, mode) VALUES (?1,?2,?3)
                   ON CONFLICT(task_id, depends_on_task_id) DO UPDATE SET mode = excluded.mode",
                rusqlite::params![task_id, on, mode],
            )
            .unwrap();
        }
        "dep_removed" => {
            let on = rowid_of(&str_of(&ev.payload, "on"));
            tx.execute(
                "DELETE FROM dep WHERE task_id = ? AND depends_on_task_id = ?",
                rusqlite::params![task_id, on],
            )
            .unwrap();
        }
        "relation_add" => {
            let to = rowid_of(&str_of(&ev.payload, "to"));
            if !task_exists(tx, to) {
                out.push(format!(
                    "event {}: relation points at {prefix}-{to}, which the log \
                     never created",
                    ev.id
                ));
                return;
            }
            tx.execute(
                "INSERT OR IGNORE INTO relation(from_task_id, to_task_id, kind, created_at)
                 VALUES (?,?,?,?)",
                rusqlite::params![task_id, to, str_of(&ev.payload, "kind"), ev.ts],
            )
            .unwrap();
        }
        "relation_removed" => {
            let to = rowid_of(&str_of(&ev.payload, "to"));
            tx.execute(
                "DELETE FROM relation WHERE from_task_id = ? AND to_task_id = ? AND kind = ?",
                rusqlite::params![task_id, to, str_of(&ev.payload, "kind")],
            )
            .unwrap();
        }
        "edit" => {
            let changes = ev.payload.get("changes").and_then(|c| c.as_object());
            let Some(changes) = changes else {
                out.push(format!("event {}: edit with no `changes` object", ev.id));
                return;
            };
            for (field, delta) in changes {
                // Whitelisted, because the column name goes into SQL. A new
                // editable field is a new arm here, which is the review the
                // property is asking for.
                if !matches!(field.as_str(), "title" | "tier" | "description") {
                    out.push(format!(
                        "event {}: edit names field `{field}`, which the replayer \
                         does not know how to project",
                        ev.id
                    ));
                    continue;
                }
                let to = delta.get("to").unwrap_or(&serde_json::Value::Null);
                let val: Option<String> = to.as_str().map(str::to_string);
                tx.execute(
                    &format!("UPDATE task SET {field} = ?1 WHERE id = ?2"),
                    rusqlite::params![val, task_id],
                )
                .unwrap();
            }
        }
        // Annotations. They are recorded *about* a task and change no row, so
        // replay is a no-op for them by design, not by omission.
        "note" | "decision" | "artifact" | "blocker" => {}
        other => out.push(format!(
            "event {}: unknown kind `{other}`. The replayer cannot know what \
             projection it writes. Add an arm for it, or add it to the \
             annotations arm if it writes none.",
            ev.id
        )),
    }
}

/// The transition arm, and the assignment projection that rides on it.
///
/// `assignment` has no events of its own: every row it holds is implied by the
/// state changes around it. That is a real claim about the log, and diffing the
/// table exactly — `outcome` included — is what tests it.
pub fn apply_state_change(
    tx: &rusqlite::Transaction,
    prefix: &str,
    ev: &Ev,
    task_id: i64,
    out: &mut Vec<String>,
) {
    let to = str_of(&ev.payload, "to");
    let Some(from) = ev.payload.get("from") else {
        out.push(format!(
            "event {}: state_change with no `from` key — either it predates the \
             origin-state fix or a caller bypassed `insert_state_change`",
            ev.id
        ));
        return;
    };

    // `from: null` is creation, and it is the only place a task comes into
    // existence. `title`, `tier` and `description` ride the birth event because
    // the INSERT that sets them has no `edit` event to say so.
    if from.is_null() {
        let display = format!("{prefix}-{task_id}");
        tx.execute(
            "INSERT INTO task(id, display_id, title, tier, description, state, created_at)
             VALUES (?,?,?,?,?,?,?)",
            rusqlite::params![
                task_id,
                display,
                str_of(&ev.payload, "title"),
                opt_str_of(&ev.payload, "tier"),
                opt_str_of(&ev.payload, "description"),
                to,
                ev.ts,
            ],
        )
        .unwrap();
        return;
    }

    let from = from.as_str().unwrap_or_default().to_string();
    // Guarded exactly the way the real transition was guarded. A mismatch means
    // the replayed store reached a different state than the log says it was in
    // — i.e. some earlier transition was never recorded — and naming both ends
    // is what makes that finding actionable.
    let n = tx
        .execute(
            "UPDATE task SET state = ?1 WHERE id = ?2 AND state = ?3",
            rusqlite::params![to, task_id, from],
        )
        .unwrap();
    if n != 1 {
        let actual: Option<String> = tx
            .query_row("SELECT state FROM task WHERE id = ?", [task_id], |r| {
                r.get(0)
            })
            .ok();
        out.push(format!(
            "event {}: task {prefix}-{task_id} `{from}` -> `{to}` could not be \
             applied; the replayed store has it in {:?}. A transition into \
             `{from}` is missing from the log.",
            ev.id, actual
        ));
        return;
    }

    // Assignment lifecycle, derived from the transition it accompanies.
    match to.as_str() {
        "assigned" => {
            let agent = ev
                .payload
                .get("agent_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| ev.agent_id.clone());
            match agent {
                Some(agent) => {
                    tx.execute(
                        "INSERT INTO assignment(task_id, agent_id, assigned_at) VALUES (?,?,?)",
                        rusqlite::params![task_id, agent, ev.ts],
                    )
                    .unwrap();
                }
                None => out.push(format!(
                    "event {}: task moved to `assigned` with no agent recorded \
                     anywhere in the event, so replay cannot open the assignment",
                    ev.id
                )),
            }
        }
        "running" if from == "assigned" => {
            tx.execute(
                "UPDATE assignment SET claimed_at = ? WHERE task_id = ? AND completed_at IS NULL",
                rusqlite::params![ev.ts, task_id],
            )
            .unwrap();
        }
        "done" => close_assignment(tx, task_id, &ev.ts, "success"),
        // `cancel` closes whatever is open, and a ready/pending task has
        // nothing open — so this is a no-op there rather than a special case.
        "cancelled" => close_assignment(tx, task_id, &ev.ts, "cancelled"),
        // A demotion to `pending` releases a claim only when it came *from* a
        // held state. Arriving at `pending` from `ready` is the freeze sweep,
        // which never touches an assignment — keying on `from` is what keeps
        // those two apart, since both carry a `via`.
        "pending" if from == "assigned" || from == "running" => {
            let outcome = match ev.payload.get("via").and_then(|v| v.as_str()) {
                Some("abandon") | Some("block") => "abandoned",
                Some("reclaim") => "reclaimed",
                other => {
                    out.push(format!(
                        "event {}: release from `{from}` with via={other:?}; replay \
                         does not know which assignment outcome that records",
                        ev.id
                    ));
                    return;
                }
            };
            close_assignment(tx, task_id, &ev.ts, outcome);
        }
        _ => {}
    }
}

pub fn task_exists(tx: &rusqlite::Transaction, id: i64) -> bool {
    tx.query_row("SELECT 1 FROM task WHERE id = ?", [id], |_| Ok(()))
        .is_ok()
}

pub fn close_assignment(tx: &rusqlite::Transaction, task_id: i64, ts: &str, outcome: &str) {
    tx.execute(
        "UPDATE assignment SET completed_at = ?1, outcome = ?2
          WHERE task_id = ?3 AND completed_at IS NULL",
        rusqlite::params![ts, outcome, task_id],
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// the diff
// ---------------------------------------------------------------------------

/// One projection to compare, keyed so a *changed* row reads as a change rather
/// than as one loss plus one gain.
pub struct Projection {
    pub name: &'static str,
    pub sql: &'static str,
    /// How many leading columns of `sql` form the row's identity.
    pub key_cols: usize,
}

/// Every table the log is required to be able to rebuild.
///
/// `created_at` and the `assignment` timestamps are compared as presence rather
/// than as values; `meta` and `default_tag` are absent on purpose. Both
/// exclusions are argued in the module header.
pub const PROJECTIONS: &[Projection] = &[
    Projection {
        name: "task",
        sql: "SELECT id, display_id, title, tier, description, state,
                     created_at IS NOT NULL AS has_created_at
                FROM task ORDER BY id",
        key_cols: 1,
    },
    Projection {
        name: "dep",
        sql: "SELECT task_id, depends_on_task_id, mode FROM dep
               ORDER BY task_id, depends_on_task_id",
        key_cols: 2,
    },
    Projection {
        name: "tag",
        sql: "SELECT task_id, name FROM tag ORDER BY task_id, name",
        key_cols: 2,
    },
    Projection {
        name: "relation",
        sql: "SELECT from_task_id, to_task_id, kind FROM relation
               ORDER BY from_task_id, to_task_id, kind",
        key_cols: 3,
    },
    Projection {
        // Keyed by (task_id, ordinal) so a task's successive assignments stay
        // distinguishable even when the same agent holds it twice.
        name: "assignment",
        sql: "SELECT task_id,
                     ROW_NUMBER() OVER (PARTITION BY task_id ORDER BY id) AS nth,
                     agent_id,
                     assigned_at IS NOT NULL  AS has_assigned_at,
                     claimed_at   IS NOT NULL AS has_claimed_at,
                     completed_at IS NOT NULL AS has_completed_at,
                     outcome
                FROM assignment ORDER BY task_id, id",
        key_cols: 2,
    },
];

/// Compares every projection and describes the differences by name.
///
/// The report is the deliverable, not the boolean: the original gaps were found
/// by reading which projection had lost which rows, so a failure that only said
/// "not equal" would have been useless.
pub fn diff_projections(src: &Path, dst: &Path) -> String {
    let s = Connection::open(src).unwrap();
    let d = Connection::open(dst).unwrap();
    let mut report = String::new();

    for p in PROJECTIONS {
        let expected = keyed(&s, p);
        let actual = keyed(&d, p);
        let mut lines = Vec::new();

        for (key, cols) in &expected {
            match actual.get(key) {
                None => lines.push(format!("  missing from replay: {}", render(cols))),
                Some(got) => {
                    for (name, want) in cols {
                        let have = got.iter().find(|(n, _)| n == name).map(|(_, v)| v);
                        if have != Some(want) {
                            lines.push(format!(
                                "  {}: column `{name}` is {} in the store but {} after replay",
                                key.join("/"),
                                want,
                                have.map(String::as_str).unwrap_or("<absent>")
                            ));
                        }
                    }
                }
            }
        }
        for (key, cols) in &actual {
            if !expected.contains_key(key) {
                lines.push(format!("  invented by replay: {}", render(cols)));
            }
        }

        if !lines.is_empty() {
            report.push_str(&format!(
                "projection `{}`: {} difference(s) ({} rows in the store, {} after replay)\n",
                p.name,
                lines.len(),
                expected.len(),
                actual.len()
            ));
            for l in lines {
                report.push_str(&l);
                report.push('\n');
            }
        }
    }
    report
}

/// Reads a projection into `key -> [(column, value)]`.
pub fn keyed(conn: &Connection, p: &Projection) -> BTreeMap<Vec<String>, Vec<(String, String)>> {
    let mut stmt = conn.prepare(p.sql).unwrap();
    let names: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    let n = names.len();
    let rows = stmt
        .query_map([], move |r| {
            let mut cols = Vec::with_capacity(n);
            for i in 0..n {
                cols.push(cell(r.get_ref(i)?));
            }
            Ok(cols)
        })
        .unwrap();

    let mut map = BTreeMap::new();
    for row in rows {
        let row = row.unwrap();
        let key = row[..p.key_cols].to_vec();
        let cols = names.iter().cloned().zip(row).collect();
        assert!(
            map.insert(key, cols).is_none(),
            "projection `{}` has a duplicate key; its key_cols is wrong",
            p.name
        );
    }
    map
}

pub fn cell(v: ValueRef<'_>) -> String {
    match v {
        ValueRef::Null => "NULL".to_string(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => f.to_string(),
        ValueRef::Text(t) => format!("{:?}", String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => format!("<{} bytes>", b.len()),
    }
}

pub fn render(cols: &[(String, String)]) -> String {
    cols.iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join(" | ")
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

pub fn qp(db: &Path) -> Command {
    let mut c = Command::cargo_bin("qp").unwrap();
    // Explicit store, always. Ambient discovery would make the test depend on
    // whatever store happens to be around it.
    c.env("QP_DB", db);
    c
}

pub fn ok(db: &Path, args: &[&str]) {
    qp(db)
        .args(args)
        .assert()
        .try_success()
        .unwrap_or_else(|e| panic!("`qp {}` failed: {e}", args.join(" ")));
}

pub fn init_store(db: &Path) {
    ok(
        db,
        &["init", "--prefix", PREFIX, "--default-tag", DEFAULT_TAG],
    );
}

pub fn assert_empty_projection(db: &Path) {
    let conn = Connection::open(db).unwrap();
    for t in ["task", "dep", "tag", "assignment", "relation", "event"] {
        let n: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            n, 0,
            "replay target must start empty, but `{t}` has {n} rows"
        );
    }
}

pub fn rows(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(sql).unwrap();
    let out = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    out
}

pub fn str_of(payload: &serde_json::Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("payload {payload} has no string `{key}`"))
        .to_string()
}

pub fn opt_str_of(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Recovers a rowid from a display id.
///
/// Mirrors `id::parse`, duplicated because `qp` is a bin-only crate with
/// nothing for an integration test to import. Only the modern
/// `<PREFIX>-<digits>` form is handled: this reads ids the source store wrote
/// during *this* test run, so the legacy form cannot occur.
pub fn rowid_of(display: &str) -> i64 {
    let (_, tail) = display
        .split_once('-')
        .unwrap_or_else(|| panic!("display id `{display}` is not <PREFIX>-<n>"));
    tail.parse()
        .unwrap_or_else(|e| panic!("display id `{display}` has no rowid: {e}"))
}
