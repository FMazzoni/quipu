//! Replay-equivalence: the event log must be able to rebuild the projection.
//!
//! This is a **property**, not a list of cases. It drives a wide script of
//! mutations against a fresh store, replays *only* that store's `event` table
//! into a second, empty store, and asserts the two projections are identical.
//! Nothing here enumerates commands, so a command added next month is covered
//! the day it is added: a write that changes state without emitting an event
//! fails this test by construction, and a new event kind whose projection the
//! replayer does not know fails it too (see `apply`'s catch-all).
//!
//! # Why it exists
//!
//! Four separate writes used to change state while emitting nothing. Fixing
//! those four was necessary and not sufficient — nothing *prevented* a fifth.
//! The diagnosis that named one of them sat open for four weeks while the gap
//! widened, because a diagnosis is a document and a document does not run in
//! CI. This does.
//!
//! # Calibration
//!
//! Measured against the pre-fix code, replaying the eight live stores lost
//! 96.4% of `tag` rows, ~84% of `task.description`, 23.9% of `dep` edges, put
//! 10 tasks in the wrong final state, and could not create one task at all.
//! Each of those symptoms maps onto a specific assertion here:
//!
//! | symptom                          | what catches it                                    |
//! |----------------------------------|----------------------------------------------------|
//! | tag rows lost                     | `tag` projection diff; the script uses `--tag` on `add`, `--default-tag` on `init`, `--tag` on `block`, and `qp tag add/rm` |
//! | `task.description` lost           | `task` projection diff, column-by-column, on tasks created with `--description` and edited with `qp edit --description` |
//! | dep edges lost                     | `dep` projection diff (including `mode`); the script writes edges through `add --depends-on`, `add --part-of`, `qp depends`, `qp contains`, and `qp block` |
//! | tasks in the wrong final state      | `task.state` in the diff, *and* the guarded replay `UPDATE` in `apply_state_change`, which refuses a transition whose recorded origin does not match the replayed store and reports it as an anomaly |
//! | a task replay cannot create         | `task` projection diff reports it as missing; the script runs `qp block`, which is the command that used to file the new task's events against the wrong task |
//!
//! `harness_detects_a_lost_projection` is the negative control: it replays the
//! same log with the `tag_added` events suppressed and asserts the diff both
//! fails and names `tag`. A property test that cannot fail is decoration.
//!
//! # Indirect transitions are the point
//!
//! Every real gap was on an *indirect* path — a state change nobody typed a
//! command for. So the script deliberately drives both directions:
//!
//! - **promotion**: completing (and separately, cancelling) a blocker so its
//!   dependents, and the contents of a container several levels up, come back
//!   to `ready`;
//! - **demotion**: hanging a blocker on a *container* so its already-`ready`
//!   contents are frozen back to `pending`.
//!
//! `script_exercised_the_indirect_paths` asserts the log actually contains both
//! before the equivalence assertion runs, so the script cannot quietly stop
//! covering them while still passing.
//!
//! # Deliberate exclusions, each with its reason
//!
//! **Store-scoped state — `meta` and `default_tag` — is excluded, and the
//! exclusion is checked rather than assumed.** `insert_event` permits a null
//! `task_id` expressly for store-scoped events and no caller has ever passed
//! one; measured across the live stores, zero such events exist. So these two
//! tables are supplied to the replay store out of band, by running `qp init`
//! with the same `--prefix` and `--default-tag` the source store was given, and
//! the test then asserts the seeded values match. `meta.project_uuid` is the one
//! value that deliberately differs: it is the store's *identity*, not its
//! content, and two stores holding the same uuid would be the bug.
//!
//! `no_store_scoped_events_exist` pins the premise. The day someone emits an
//! event with a null `task_id`, that test fails and points here — at which
//! point the seed is no longer the right model and the replayer must apply
//! those events instead.
//!
//! **Timestamps are compared as presence, not as values.** `event.ts` and the
//! `assigned_at`/`claimed_at`/`completed_at`/`created_at` columns are
//! independent clock reads of the same instant, from two different clocks
//! (SQLite's `strftime` and `time::now_rfc3339`). Requiring equality would
//! assert something the log was never meant to carry. What it *must* carry is
//! the shape: which assignments exist, for whom, whether they were claimed, and
//! how they ended. Those are compared exactly, `outcome` included.
//!
//! **`event.agent_id` is not a projection** and is not diffed. It is
//! attribution on the log itself; it reaches the projection only where a
//! command writes it into `assignment.agent_id`, which *is* diffed.
//!
//! # Known limit, stated rather than fixed
//!
//! Backfill is impossible. History written before the origin state was recorded
//! is permanently ambiguous — a reconstruction would be a guess wearing an audit
//! trail's clothes. This test guards the **future** only: it proves a log
//! written by today's code is replayable, and says nothing about logs already
//! on disk.

mod common;

use common::*;
use rusqlite::Connection;
// ---------------------------------------------------------------------------
// the property
// ---------------------------------------------------------------------------

#[test]
fn event_log_replays_to_an_identical_projection() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.sqlite");
    let dst = tmp.path().join("replay.sqlite");

    init_store(&src);
    run_mutation_script(&src);
    init_store(&dst);
    assert_empty_projection(&dst);

    let anomalies = replay(&src, &dst, |_| true);
    let diffs = diff_projections(&src, &dst);

    let mut report = String::new();
    if !anomalies.is_empty() {
        report.push_str("replay could not apply some events:\n");
        for a in &anomalies {
            report.push_str(&format!("  {a}\n"));
        }
    }
    report.push_str(&diffs);
    assert!(
        report.is_empty(),
        "the event log does not rebuild the projection.\n\
         Some write changed state without recording it, or recorded it in a form \
         replay cannot apply.\n\n{report}"
    );
}

/// Negative control: prove the harness can actually fail, and that its failure
/// names the projection that lost rows.
///
/// Without this, a replayer that silently did nothing and a diff that compared
/// nothing would both pass the test above.
#[test]
fn harness_detects_a_lost_projection() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.sqlite");
    let dst = tmp.path().join("replay.sqlite");

    init_store(&src);
    run_mutation_script(&src);
    init_store(&dst);

    // Simulate exactly the defect this test exists to catch: a write that
    // changed the projection without leaving an event behind.
    let anomalies = replay(&src, &dst, |ev| ev.kind != "tag_added");
    assert!(anomalies.is_empty(), "unexpected anomalies: {anomalies:?}");
    let diffs = diff_projections(&src, &dst);

    assert!(
        diffs.contains("projection `tag`"),
        "suppressing every tag_added event must be reported against the `tag` \
         projection, so the reader knows where to look. got:\n{diffs}"
    );
    assert!(
        diffs.contains("missing from replay"),
        "the diff must say the rows are missing, not merely that something \
         differs. got:\n{diffs}"
    );
}

/// The script must keep exercising the paths where every real gap was found.
///
/// Indirect transitions — a task moved by something other than a command naming
/// it — are where all four confirmed holes lived. A script driving only direct
/// commands passes the equivalence assertion while the real defect survives, so
/// the coverage is asserted rather than assumed.
#[test]
fn script_exercised_the_indirect_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.sqlite");
    init_store(&src);
    run_mutation_script(&src);

    let conn = Connection::open(&src).unwrap();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };

    // Promotion nobody asked for: a blocker finished, and something else moved.
    let promotions = count(
        "SELECT COUNT(*) FROM event WHERE kind = 'state_change'
           AND json_extract(payload,'$.to') = 'ready'
           AND json_extract(payload,'$.from') = 'pending'
           AND json_extract(payload,'$.via') IN ('complete','cancel')",
    );
    assert!(
        promotions > 0,
        "script no longer causes an indirect promotion (a blocker resolving and \
         unblocking someone else); that is the path `refresh_ready` used to take \
         without logging"
    );

    // Demotion nobody asked for: a blocker hung on a container, freezing its
    // contents. This is the containment half, and it reaches tasks the command
    // never named.
    let demotions = count(
        "SELECT COUNT(*) FROM event WHERE kind = 'state_change'
           AND json_extract(payload,'$.to') = 'pending'
           AND json_extract(payload,'$.from') = 'ready'
           AND json_extract(payload,'$.via') IN ('blocks','contains','block')",
    );
    assert!(
        demotions > 0,
        "script no longer causes an indirect demotion (a blocker on a container \
         freezing its contents); that is the `demote_frozen` path"
    );

    // The command that mints a task as a side effect. Its new task used to have
    // no creation event at all, so replay could not bring it into existence.
    assert!(
        count("SELECT COUNT(*) FROM event WHERE kind = 'blocker'") > 0,
        "script no longer runs `qp block`"
    );
}

/// Pins the premise behind excluding `meta` and `default_tag` from the replay.
///
/// `insert_event` allows a null `task_id` for store-scoped events. Nothing has
/// ever written one, which is why this test can seed those two tables out of
/// band instead of replaying them. If this fails, the log has grown
/// store-scoped events and the exclusion documented in the module header is no
/// longer true — extend `apply` to project them rather than relaxing this.
#[test]
fn no_store_scoped_events_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.sqlite");
    init_store(&src);
    run_mutation_script(&src);

    let conn = Connection::open(&src).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM event WHERE task_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        n, 0,
        "a store-scoped event now exists. The replay store seeds `meta` and \
         `default_tag` from `qp init` on the assumption that nothing logs them; \
         that assumption just broke."
    );
}

/// The out-of-band seed has to reproduce the store-scoped state exactly, or the
/// exclusion is hiding a difference rather than documenting one.
///
/// `project_uuid` is the deliberate exception: it identifies the store, and two
/// stores sharing one would be the defect.
#[test]
fn store_scoped_state_is_reproduced_by_the_seed() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.sqlite");
    let dst = tmp.path().join("replay.sqlite");
    init_store(&src);
    init_store(&dst);

    let s = Connection::open(&src).unwrap();
    let d = Connection::open(&dst).unwrap();
    let meta = |c: &Connection, k: &str| -> String {
        c.query_row("SELECT value FROM meta WHERE key = ?", [k], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(meta(&s, "display_prefix"), meta(&d, "display_prefix"));
    assert_eq!(meta(&s, "schema_version"), meta(&d, "schema_version"));
    assert_ne!(
        meta(&s, "project_uuid"),
        meta(&d, "project_uuid"),
        "project_uuid is store identity, not content; two stores must not share one"
    );
    assert_eq!(
        rows(&s, "SELECT name FROM default_tag ORDER BY name"),
        rows(&d, "SELECT name FROM default_tag ORDER BY name")
    );
}
