//! `qp report` — emit a structured JSON snapshot of the qp store.
//!
#![doc = include_str!("../../docs/modules/report.md")]

use crate::outcome::Outcome;
use crate::{db, id, lease, store};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::io::Write;
use time::{
    format_description::well_known::Rfc3339, macros::format_description, OffsetDateTime, UtcOffset,
};

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportArgs {
    /// Filter events to this window. Accepts `24h`, `7d`, or RFC3339 date (e.g. `2026-05-20`).
    #[arg(long)]
    #[serde(default)]
    pub since: Option<String>,
    /// Scope to a wave: restrict to the given task + its transitive dep subtree.
    #[arg(long)]
    #[serde(default)]
    pub wave: Option<String>,
    /// Emit JSON. This is the only supported output format now; the flag is
    /// accepted for backward compatibility (existing scripts pass it) but has
    /// no effect — report always emits JSON.
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
    /// Write to this path instead of stdout.
    #[arg(long)]
    #[serde(skip)]
    pub output: Option<std::path::PathBuf>,
    /// Single-ticket mode: emit the full detail (incl. parents/children/uncapped events) for one ticket.
    #[arg(long, conflicts_with = "all_tickets")]
    #[serde(default)]
    pub ticket: Option<String>,
    /// Bulk mode: emit a JSON array of per-ticket detail objects, scoped by `--since`/`--wave`.
    #[arg(long = "all-tickets", conflicts_with = "ticket")]
    #[serde(default)]
    pub all_tickets: bool,
}

impl crate::route::Served for ReportArgs {
    const VERB: &'static str = "report";
    type Out = ReportView;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

/// The snapshot, in whichever of the three shapes the flags selected.
///
/// `human()` is the JSON, because `report` has no prose form and never had one:
/// `--json` is documented as accepted-and-ignored. That makes the two `emit`
/// branches byte-identical here, which is the honest encoding of "this command
/// emits JSON".
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ReportView(#[serde(deserialize_with = "body_is_a_report")] Value);

/// Refuses a response body that is not one of `report`'s three payloads.
///
/// The one view whose shape depends on its own arguments: `--ticket` renders a
/// ticket detail object, `--all-tickets` an array of them, and the default a
/// board. So the check is a disjunction rather than a single shape — which is
/// still strictly better than the nothing that was there, because `200 {}` and
/// `200 null` are refused by all three arms. Making it exact would mean
/// threading the caller's flags into a `deserialize_with`, and the flags are
/// `#[serde(skip)]` precisely so they never reach the wire.
///
/// See `wire::expect_array` for what this is defending against.
fn body_is_a_report<'de, D>(d: D) -> std::result::Result<Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let v = Value::deserialize(d)?;
    if v.is_array() {
        return Ok(v);
    }
    for shape in [BOARD_KEYS, TICKET_KEYS] {
        if crate::wire::expect_object(v.clone(), shape).is_ok() {
            return Ok(v);
        }
    }
    Err(D::Error::custom(format!(
        "expected `report`'s board object (with {}), a ticket detail (with {}), \
         or an array of ticket details; this is not a response this client \
         understands, and rendering it would show a blank board rather than fail",
        BOARD_KEYS.join(", "),
        TICKET_KEYS.join(", ")
    )))
}

/// The keys `collect_json` always writes.
const BOARD_KEYS: &[&str] = &["tasks", "events", "deps"];
/// The keys `ticket_detail_json` always writes.
const TICKET_KEYS: &[&str] = &["display_id", "state", "events"];

/// A minimal valid sample; see `list.rs` for why this is not derived.
#[cfg(test)]
impl Default for ReportView {
    fn default() -> Self {
        ReportView(serde_json::json!({"tasks": [], "events": [], "deps": []}))
    }
}

impl Outcome for ReportView {
    fn human(&self) -> String {
        serde_json::to_string(&self.0).unwrap_or_default()
    }
}

/// Fetches the snapshot, then writes it where the *caller* asked.
///
/// `--output` names a path on this machine's disk, so it is `#[serde(skip)]` and
/// the write happens client-side: served, it would drop the batch on the
/// server's filesystem and hand the caller an empty directory. That is the same
/// test the wire contract applies to `export` and `init` — ask whose disk the
/// path is on — and it is why `report` calls `route::fetch` and does its own
/// output rather than going through `route::dispatch`.
pub fn run(ctx: &crate::route::Ctx, a: ReportArgs) -> Result<()> {
    let (json, output) = (a.json, a.output.clone());
    let view = crate::route::fetch::<ReportArgs>(ctx, a)?;
    match output {
        Some(path) => write_output(Some(&path), &view.human()),
        None => crate::outcome::emit(json, &view),
    }
}

fn exec(db_path: &std::path::Path, a: ReportArgs) -> Result<ReportView> {
    let conn = db::open(db_path)?;
    let since_iso = a.since.as_deref().map(parse_since).transpose()?;
    let subtree = a
        .wave
        .as_deref()
        .map(|t| resolve_subtree(&conn, t))
        .transpose()?;

    // Per-ticket mode: full detail for one ticket.
    if let Some(tref) = a.ticket.as_deref() {
        let resolved = id::resolve_full(&conn, tref)?;
        let detail = collect_ticket(&conn, resolved.id)?;
        return Ok(ReportView(ticket_detail_json(&detail)));
    }

    // Bulk mode: array of per-ticket detail, scoped.
    if a.all_tickets {
        let scope_ids = ticket_ids_in_scope(&conn, since_iso.as_deref(), subtree.as_ref())?;
        let mut arr = Vec::with_capacity(scope_ids.len());
        for tid in scope_ids {
            let detail = collect_ticket(&conn, tid)?;
            arr.push(ticket_detail_json(&detail));
        }
        return Ok(ReportView(Value::Array(arr)));
    }

    // Default: board payload.
    Ok(ReportView(collect_json(
        &conn,
        since_iso.as_deref(),
        subtree.as_ref(),
    )?))
}

fn write_output(path: Option<&std::path::Path>, body: &str) -> Result<()> {
    if let Some(path) = path {
        let mut f = std::fs::File::create(path)?;
        f.write_all(body.as_bytes())?;
        f.write_all(b"\n")?;
    } else {
        println!("{body}");
    }
    Ok(())
}

// ---------- Per-ticket collection ----------

struct TicketDetail {
    display_id: String,
    title: String,
    state: String,
    tier: Option<String>,
    agent: Option<String>,
    description: Option<String>,
    created_at: Option<String>,
    tags: Vec<String>,
    parents: Vec<(String, String, String, String)>, // display_id, title, state, mode — this depends on
    children: Vec<(String, String, String, String)>, // display_id, title, state, mode — depend on this
    events: Vec<store::EventRow>,                    // chronological asc, uncapped
}

fn ticket_detail_json(t: &TicketDetail) -> Value {
    // `parents`/`children` name the *dep* direction: a "parent" is something
    // this ticket depends on. That reads fine for blocking and exactly backwards
    // for containment, where the things a container depends on are its contents
    // — so a consumer rendering `mode: contains` under a "parents" heading
    // inverts the hierarchy. The two names are kept for compatibility and
    // `mode` is carried on each entry, but the unambiguous lists below are what
    // a renderer should reach for.
    let edge = |(did, title, state, mode): &(String, String, String, String)| serde_json::json!({"display_id": did, "title": title, "state": state, "mode": mode});
    let parents: Vec<Value> = t.parents.iter().map(edge).collect();
    let children: Vec<Value> = t.children.iter().map(edge).collect();
    let pick = |v: &Vec<(String, String, String, String)>, want: &str| -> Vec<Value> {
        v.iter()
            .filter(|(_, _, _, m)| m == want)
            .map(edge)
            .collect()
    };
    let contains = pick(&t.parents, crate::db::MODE_CONTAINS);
    let blocked_by = pick(&t.parents, crate::db::MODE_BLOCKS);
    let part_of = pick(&t.children, crate::db::MODE_CONTAINS);
    let blocks = pick(&t.children, crate::db::MODE_BLOCKS);
    let events: Vec<Value> = t
        .events
        .iter()
        .map(|e| {
            serde_json::json!({
                "ts": e.ts,
                "kind": e.kind,
                "agent_id": e.agent,
                "payload": e.payload,
            })
        })
        .collect();
    serde_json::json!({
        "display_id": t.display_id,
        "title": t.title,
        "state": t.state,
        "tier": t.tier,
        "agent": t.agent,
        "description": t.description,
        "created_at": t.created_at,
        "tags": t.tags,
        "parents": parents,
        "children": children,
        // Named from this ticket's point of view, so no reader has to know
        // which way a dep edge points to render the graph correctly.
        "contains": contains,
        "part_of": part_of,
        "blocked_by": blocked_by,
        "blocks": blocks,
        "events": events,
    })
}

fn collect_ticket(conn: &rusqlite::Connection, tid: i64) -> Result<TicketDetail> {
    let (display_id, title, state, tier, description, created_at): (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = conn.query_row(
        "SELECT display_id, title, state, tier, description, created_at FROM task WHERE id = ?1",
        [tid],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    let agent = store::latest_agent(conn, tid)?;
    let mut tags = store::tags_by_task(conn, &[tid])?
        .remove(&tid)
        .unwrap_or_default();
    tags.sort();

    let mut p_stmt = conn.prepare(
        "SELECT t.display_id, t.title, t.state, d.mode FROM dep d JOIN task t ON t.id = d.depends_on_task_id
          WHERE d.task_id = ?1 ORDER BY t.id")?;
    let parents: Vec<(String, String, String, String)> = p_stmt
        .query_map([tid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut c_stmt = conn.prepare(
        "SELECT t.display_id, t.title, t.state, d.mode FROM dep d JOIN task t ON t.id = d.task_id
          WHERE d.depends_on_task_id = ?1 ORDER BY t.id",
    )?;
    let children: Vec<(String, String, String, String)> = c_stmt
        .query_map([tid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<_, _>>()?;

    // Full timeline for this ticket, oldest-first, uncapped.
    let events = store::events(
        conn,
        &store::EventFilter {
            task_id: Some(tid),
            ..Default::default()
        },
    )?;

    Ok(TicketDetail {
        display_id,
        title,
        state,
        tier,
        agent,
        description,
        created_at,
        tags,
        parents,
        children,
        events,
    })
}

fn ticket_ids_in_scope(
    conn: &rusqlite::Connection,
    since_iso: Option<&str>,
    subtree: Option<&HashSet<i64>>,
) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT id FROM task ORDER BY id ASC")?;
    let mut ids: Vec<i64> = stmt
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<Result<_, _>>()?;
    if let Some(set) = subtree {
        ids.retain(|i| set.contains(i));
    }
    if let Some(s) = since_iso {
        // Keep tickets with any event in the window.
        let mut e = conn
            .prepare("SELECT DISTINCT task_id FROM event WHERE task_id IS NOT NULL AND ts >= ?1")?;
        let recent: HashSet<i64> = e
            .query_map([s], |r| r.get::<_, i64>(0))?
            .collect::<Result<_, _>>()?;
        ids.retain(|i| recent.contains(i));
    }
    Ok(ids)
}

// ---------- JSON board payload ----------

fn collect_json(
    conn: &rusqlite::Connection,
    since_iso: Option<&str>,
    subtree: Option<&HashSet<i64>>,
) -> Result<Value> {
    // Tasks: same shape as `qp list --json`.
    let core = store::tasks(conn, &store::TaskFilter::default())?;

    // Filter by subtree if scoped.
    let core: Vec<store::TaskRow> = match subtree {
        Some(set) => core.into_iter().filter(|t| set.contains(&t.id)).collect(),
        None => core,
    };

    let task_ids: Vec<i64> = core.iter().map(|t| t.id).collect();
    let mut tags_by = store::tags_by_task(conn, &task_ids)?;
    let mut blockers_by = store::unresolved_blockers_by_task(conn, &task_ids)?;
    let mut last_event_by = store::last_event_by_task(conn, &task_ids)?;
    // The dashboard reads this payload, and a ticket stranded by a machine that
    // died has to be visible in it — otherwise the board shows a `running` row
    // indistinguishable from work in progress. One statement for the whole
    // page, and the same `LIVE_HOLD` predicate `reclaim` breaks holds with, so
    // what a renderer calls stale and what `reclaim` will take cannot drift.
    let holds = lease::holds_by_task(conn, &task_ids)?;

    let mut tasks: Vec<Value> = Vec::with_capacity(core.len());
    for t in core {
        let hold = holds.get(&t.id);
        let mut obj = serde_json::json!({
            "id": t.id, "display_id": t.display_id, "title": t.title, "state": t.state, "tier": t.tier,
            "agent": t.agent,
            "tags": tags_by.remove(&t.id).unwrap_or_default(),
            "blocked_by": blockers_by.remove(&t.id).unwrap_or_default(),
            "last_event": last_event_by.remove(&t.id),
            // Spelled exactly as `wave --json` and `list --json` spell it — a
            // flag beside the row and the whole hold object under `hold` — so a
            // renderer written against one view reads the others unchanged.
            "stale": hold.is_some_and(|h| h.stale),
            "hold": hold.map(lease::Hold::to_json),
        });
        if let Some(d) = t.description {
            obj.as_object_mut()
                .unwrap()
                .insert("description".into(), Value::String(d));
        }
        tasks.push(obj);
    }

    // Events: same shape as `qp timeline --json`.
    let evt_rows = store::events(conn, &store::EventFilter::default())?;

    let did_to_id: Option<std::collections::HashMap<String, i64>> = if subtree.is_some() {
        let mut m = std::collections::HashMap::new();
        let mut s = conn.prepare("SELECT id, display_id FROM task")?;
        for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, did) = r?;
            m.insert(did, id);
        }
        Some(m)
    } else {
        None
    };

    let mut events: Vec<Value> = Vec::new();
    for e in evt_rows {
        // `--since` scope: skip events before the window.
        if let Some(s) = since_iso {
            if e.ts.as_str() < s {
                continue;
            }
        }
        // Subtree scope: skip events whose task isn't in the subtree.
        if let (Some(ref map), Some(set)) = (&did_to_id, subtree) {
            match e.task.as_ref().and_then(|d| map.get(d)) {
                Some(tid) if set.contains(tid) => {}
                _ => continue,
            }
        }
        events.push(crate::cmd::render::event_json(&e));
        if events.len() >= 200 {
            break;
        }
    }

    // Deps: all dep edges in scope (both tasks must be in subtree if scoped).
    //
    // `mode` rides along on every edge so a board can draw hierarchy and
    // blocking differently without re-querying or guessing. Omitting it is what
    // forced consumers to treat a wave as blocked by its own slices.
    let mut dep_stmt = conn.prepare(
        "SELECT tf.display_id, tt.display_id, d.mode
           FROM dep d
           JOIN task tf ON tf.id = d.task_id
           JOIN task tt ON tt.id = d.depends_on_task_id
          ORDER BY d.task_id ASC",
    )?;
    let deps_raw: Vec<(String, String, String)> = dep_stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<_, _>>()?;

    let task_dids: std::collections::HashSet<&str> = tasks
        .iter()
        .filter_map(|t| t["display_id"].as_str())
        .collect();

    let deps: Vec<Value> = deps_raw
        .into_iter()
        .filter(|(from, to, _)| {
            task_dids.contains(from.as_str()) && task_dids.contains(to.as_str())
        })
        .map(|(from, to, mode)| serde_json::json!({"from": from, "to": to, "mode": mode}))
        .collect();

    Ok(serde_json::json!({
        "tasks": tasks,
        "events": events,
        "deps": deps,
    }))
}

// ---------- duration parsing ----------

const SECONDS_PER_HOUR: i64 = 3_600;
const SECONDS_PER_DAY: i64 = 86_400;

/// The wire shape every `--since` bound is emitted in.
///
/// RFC3339 at millisecond precision, matching the
/// `strftime('%Y-%m-%dT%H:%M:%fZ', 'now')` SQLite writes into `event.ts`.
/// Fixed width and an always-present fraction are what make byte ordering
/// agree with instant ordering.
const SINCE_FORMAT: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

/// Render an instant as the canonical `--since` bound.
///
/// Every failure — including a pre-year-0 instant, which has no RFC3339
/// rendering — maps onto the caller's out-of-range error.
fn emit_bound(at: OffsetDateTime, s: &str) -> Result<String> {
    let out_of_range = || db::invalid_input(format!("bad --since `{s}` (out of range)"));
    if at.year() < 0 {
        return Err(out_of_range());
    }
    at.format(SINCE_FORMAT).map_err(|_| out_of_range())
}

/// Parse `--since` into an RFC3339 UTC string.
///
/// Accepts `Nh`, `Nd`, or an RFC3339-ish date. Every accepted form is *parsed*
/// and re-emitted in UTC at millisecond precision, so the result really is the
/// UTC instant the caller asked for and string ordering against `event.ts` is
/// instant ordering — the property both consumers rely on (the SQL `ts >= ?1`
/// filter and the in-memory `e.ts.as_str() < s` skip).
///
/// All three forms go through [`emit_bound`], so all three land on the same
/// fixed-width `strftime('%Y-%m-%dT%H:%M:%fZ', 'now')` shape SQLite writes into
/// `event.ts`. That matters twice over. A *fractionless* bound sorts above
/// every event in the same second, because `Z` (0x5A) sorts above `.` (0x2E) —
/// and `Rfc3339` emits exactly that whenever the nanoseconds happen to be zero,
/// as well as a truncated `.5` whenever they are a multiple of 1e8. Either one
/// silently drops events inside the requested window. Sub-millisecond input is
/// truncated instead, which can only widen the window, never narrow it.
///
/// Instants that cannot be rendered in this shape — pre-year-0 (reachable from
/// a positive offset near year 0) or past year 9999 (reachable from a negative
/// offset, or from `Nh`/`Nd` arithmetic) — are rejected as out of range rather
/// than emitted as something that is not RFC3339.
fn parse_since(s: &str) -> Result<String> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix('h') {
        let hours: i64 = rest
            .parse()
            .map_err(|_| db::invalid_input(format!("bad --since `{s}` (expected Nh)")))?;
        let now = OffsetDateTime::now_utc();
        // `Duration::hours` and `-` both panic on overflow, and a timestamp
        // that lands before year 0 has no RFC3339 rendering. All three are
        // reachable from `--since`, so all three fold into the same error.
        let then = hours
            .checked_mul(SECONDS_PER_HOUR)
            .map(time::Duration::seconds)
            .and_then(|d| now.checked_sub(d))
            .ok_or_else(|| db::invalid_input(format!("bad --since `{s}` (out of range)")))?;
        return emit_bound(then, s);
    }
    if let Some(rest) = s.strip_suffix('d') {
        let days: i64 = rest
            .parse()
            .map_err(|_| db::invalid_input(format!("bad --since `{s}` (expected Nd)")))?;
        let now = OffsetDateTime::now_utc();
        let then = days
            .checked_mul(SECONDS_PER_DAY)
            .map(time::Duration::seconds)
            .and_then(|d| now.checked_sub(d))
            .ok_or_else(|| db::invalid_input(format!("bad --since `{s}` (out of range)")))?;
        return emit_bound(then, s);
    }
    // RFC3339 — or a bare YYYY-MM-DD which we widen to T00:00:00Z. The widened
    // form goes through the same parse as any other RFC3339 value: shape alone
    // ("20xx-aa-bb" is 10 bytes with dashes in the right places) is not
    // validation, and skipping it made strictness depend on string length.
    let widened;
    let candidate = if s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        widened = format!("{s}T00:00:00Z");
        widened.as_str()
    } else {
        s
    };
    let at = OffsetDateTime::parse(candidate, &Rfc3339).map_err(|_| {
        db::invalid_input(format!("bad --since `{s}` (expected Nh, Nd, or RFC3339)"))
    })?;
    // Normalise: an accepted `+05:00` value names a UTC instant, but compared
    // verbatim as text it is neither that instant nor any other. The shift is
    // *checked* — `to_offset` panics when it pushes past the representable
    // range, which `9999-12-31T23:59:59-00:01` does.
    let utc = at
        .checked_to_offset(UtcOffset::UTC)
        .ok_or_else(|| db::invalid_input(format!("bad --since `{s}` (out of range)")))?;
    emit_bound(utc, s)
}

fn resolve_subtree(conn: &rusqlite::Connection, task: &str) -> Result<HashSet<i64>> {
    let root = id::resolve_full(conn, task)?.id;
    store::subtree_ids(conn, root)
}

#[cfg(test)]
mod tests {
    use super::{parse_since, SINCE_FORMAT};
    use time::{OffsetDateTime, PrimitiveDateTime};

    /// Read a bound back as an instant.
    ///
    /// Also pins that the emitted shape is round-trippable, which a
    /// fractionless or variable-width one is not.
    fn as_instant(bound: &str) -> OffsetDateTime {
        PrimitiveDateTime::parse(bound, SINCE_FORMAT)
            .unwrap_or_else(|e| panic!("bound {bound} is not in the canonical shape: {e}"))
            .assume_utc()
    }

    /// Every bound is fixed-width with all three subsecond digits present,
    /// whatever branch produced it.
    fn assert_canonical(bound: &str, probe: &str) {
        assert_eq!(bound.len(), 24, "--since {probe} -> {bound}: wrong width");
        assert_eq!(
            &bound[19..20],
            ".",
            "--since {probe} -> {bound}: missing subsecond fraction"
        );
        assert!(
            bound[20..23].bytes().all(|b| b.is_ascii_digit()),
            "--since {probe} -> {bound}: fraction is not three digits"
        );
        assert!(bound.ends_with('Z'), "--since {probe} -> {bound}: not UTC");
        // The discriminator. `event.ts` always carries three subsecond digits,
        // so a bound written to the whole second sorts ABOVE every event in
        // the second it names — `Z` (0x5A) is above `.` (0x2E) — and silently
        // drops them. With the fraction present this holds by construction.
        let latest_in_second = format!("{}.999Z", &bound[..19]);
        assert!(
            latest_in_second.as_str() >= bound,
            "--since {probe} -> {bound}: excludes events inside its own second"
        );
        let ninety_nine_ms = format!("{}.099Z", &bound[..19]);
        assert!(
            ninety_nine_ms.as_str() >= bound || &bound[20..23] > "099",
            "--since {probe} -> {bound}: excludes an event 99ms into its own second"
        );
    }

    /// `Nh`, `Nd` and the date forms must all emit the same shape.
    ///
    /// The duration branches used to go through `Rfc3339`, which strips
    /// trailing subsecond zeros and omits the fraction entirely at
    /// nanos == 0 — rare, but a silently wrong answer when it hits.
    #[test]
    fn every_since_form_emits_a_millisecond_bound() {
        for probe in [
            "0h",
            "1h",
            "24h",
            "1000000h",
            "0d",
            "7d",
            "-1h",
            "2026-05-20",
            "2026-05-20T10:00:00Z",
            "2026-05-20T10:00:00+05:00",
            "2026-05-20T10:00:00.1Z",
            "2026-05-20T10:00:00.123456789Z",
        ] {
            let bound = parse_since(probe).unwrap_or_else(|e| panic!("--since {probe}: {e}"));
            assert_canonical(&bound, probe);
        }
    }

    /// Normalisation is value-preserving.
    ///
    /// Offsets fold into UTC, sub-millisecond input truncates (widening the
    /// window, never narrowing it), and a bare date means midnight UTC.
    #[test]
    fn date_forms_normalise_to_the_instant_they_name() {
        let cases = [
            ("2026-05-20", "2026-05-20T00:00:00.000Z"),
            ("2026-05-20T10:00:00Z", "2026-05-20T10:00:00.000Z"),
            ("2026-05-20T10:00:00+05:00", "2026-05-20T05:00:00.000Z"),
            ("2026-05-20T10:00:00-05:00", "2026-05-20T15:00:00.000Z"),
            ("2026-05-20T10:00:00.1Z", "2026-05-20T10:00:00.100Z"),
            ("2026-05-20T10:00:00.123456789Z", "2026-05-20T10:00:00.123Z"),
        ];
        for (probe, want) in cases {
            assert_eq!(parse_since(probe).unwrap(), want, "--since {probe}");
        }
    }

    /// Reshaping the `Nh`/`Nd` bound must not move the instant it names.
    ///
    /// The only permitted difference from the old rendering is truncation
    /// below the millisecond `event.ts` never records anyway.
    #[test]
    fn duration_forms_still_name_the_instant_they_used_to() {
        for (probe, back_seconds) in [("1h", 3_600i64), ("24h", 86_400), ("7d", 604_800)] {
            let before = OffsetDateTime::now_utc();
            let bound = as_instant(&parse_since(probe).unwrap());
            let after = OffsetDateTime::now_utc();
            let d = time::Duration::seconds(back_seconds);
            // Truncation can only move the bound EARLIER, by under a millisecond.
            assert!(
                bound <= after - d && bound >= before - d - time::Duration::milliseconds(1),
                "--since {probe} -> {bound} is not now - {back_seconds}s"
            );
        }
    }

    /// Instants with no rendering in the canonical shape are rejected.
    ///
    /// They are not emitted as something that is not RFC3339, and not
    /// silently clamped into range.
    #[test]
    fn unrepresentable_shifts_are_rejected() {
        for probe in [
            // A negative offset pushes past year 9999. `to_offset` panicked here.
            "9999-12-31T23:59:59-00:01",
            "9999-12-31T23:00:00-05:00",
            // A positive offset pushes below year 0, which renders with a
            // leading `-` and is not RFC3339.
            "0000-01-01T00:00:00+05:00",
            "0000-01-01T00:00:00+00:01",
        ] {
            let err = parse_since(probe).expect_err("must be rejected");
            assert!(
                err.to_string().contains("out of range"),
                "--since {probe}: {err}"
            );
        }
        // The other side of each edge still works.
        for probe in [
            "9999-12-31T23:59:59+00:01",
            "0000-01-01T00:00:00Z",
            "0000-01-01T00:00:00-05:00",
        ] {
            assert_canonical(&parse_since(probe).unwrap(), probe);
        }
    }
}
