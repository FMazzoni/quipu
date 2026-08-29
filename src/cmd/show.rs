//! `qp show <task>` — single-ticket detail view.
//!
#![doc = include_str!("../../docs/modules/show.md")]

use crate::outcome::Outcome;
use crate::{db, id, lease, store};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How many events the human view prints before it starts saying "… N older".
///
/// A readability limit, not a data contract: nothing downstream may assume a
/// ticket has at most this many events, and `--json` ignores it entirely. The
/// number is arbitrary — it exists so a ticket with hundreds of events still
/// fits on a screen. Raise or lower it freely; the only rule is that human mode
/// must keep announcing what it dropped, because a silently truncated list is
/// what made the old cap look like a promise about the data.
const HUMAN_EVENT_TAIL: usize = 10;

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShowArgs {
    /// Task display id (e.g. QP-26)
    pub task: String,
    /// Emit JSON instead of human-readable text.
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
}

impl crate::route::Served for ShowArgs {
    const VERB: &'static str = "show";
    type Out = ShowView;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
}

/// One ticket, whole: core record, edges, hold, and every event.
///
/// The events are deliberately uncapped here and trimmed in `human` — the
/// prose says how many it dropped, which it can only do while it still holds
/// the total. Capping server-side would make `--json` and the wire disagree
/// with each other about what a ticket's history is.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct ShowView(Value);

/// Comma-joined display ids from one of the edge arrays.
fn ids(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect()
}

impl Outcome for ShowView {
    fn human(&self) -> String {
        let v = &self.0;
        let str_of = |k: &str| v[k].as_str().unwrap_or("");
        let tags = ids(v, "tags");
        let tag_summary = if tags.is_empty() {
            String::from("-")
        } else {
            tags.join(", ")
        };
        let agent = v["agent"].as_str();
        let mut out: Vec<String> = Vec::new();
        // The header carries exactly `list`'s columns in `list`'s order — id,
        // state, agent, tags — so the two commands can never disagree
        // positionally. Adding a column here that `list` does not have is what
        // caused QP-154: `tier` sat where a `list` reader expects AGENT, and
        // its `-` read as "nobody holds this". Any field `list` does not carry
        // belongs in the labelled block below, not in this line.
        out.push(format!(
            "{}  {}  {}  {}",
            str_of("display_id"),
            str_of("state"),
            agent.unwrap_or("-"),
            tag_summary
        ));
        out.push(str_of("title").to_string());
        out.push(String::new());
        out.push(format!("  agent: {}", agent.unwrap_or("—")));
        let hold = &v["hold"];
        if hold.is_object() {
            let host = hold["host"].as_str().unwrap_or("unknown host");
            let who = hold["agent"].as_str().unwrap_or("");
            let stale = hold["stale"].as_bool().unwrap_or(false);
            out.push(match (hold["expires_at"].as_str(), stale) {
                (Some(e), true) => {
                    format!("  hold: STALE — {who} on {host} went quiet before {e}")
                }
                (Some(e), false) => {
                    format!("  hold: {who} on {host} (stale if quiet past {e})")
                }
                (None, _) => format!("  hold: {who} on {host} (no stated expiry)"),
            });
        }
        if let Some(t) = v["tier"].as_str() {
            out.push(format!("  tier: {t}"));
        }
        if let Some(c) = v["created_at"].as_str() {
            out.push(format!("  created: {c}"));
        }
        if !tags.is_empty() {
            out.push(format!("  tags: {}", tags.join(", ")));
        }
        for (key, label) in [
            ("part_of", "part_of"),
            ("contains", "contains"),
            ("blocked_by", "blocked_by"),
        ] {
            let list = ids(v, key);
            if !list.is_empty() {
                out.push(format!("  {label}: {}", list.join(", ")));
            }
        }
        for f in v["frozen_by"].as_array().into_iter().flatten() {
            out.push(format!(
                "  frozen_by: {} (blocking {})",
                f["blocker"].as_str().unwrap_or(""),
                f["container"].as_str().unwrap_or("")
            ));
        }
        if let Some(d) = v["description"].as_str().filter(|s| !s.is_empty()) {
            out.push(String::new());
            out.extend(wrap_text(d, 80));
        }
        out.push(String::new());

        let events: Vec<&Value> = v["recent_events"]
            .as_array()
            .into_iter()
            .flatten()
            .collect();
        let total = events.len();
        if total > HUMAN_EVENT_TAIL {
            out.push(format!("Recent events ({HUMAN_EVENT_TAIL} of {total}):"));
            // Named where the dropped events would have been printed: older
            // than everything below it.
            out.push(format!(
                "  … {} older (qp timeline {})",
                total - HUMAN_EVENT_TAIL,
                str_of("display_id")
            ));
        } else {
            out.push(format!("Recent events ({total}):"));
        }
        // `recent_events` is oldest-first, so the tail is the newest window and
        // is already in reading order.
        for e in events.iter().skip(total.saturating_sub(HUMAN_EVENT_TAIL)) {
            let kind = e["kind"].as_str().unwrap_or("");
            let ts = e["ts"].as_str().unwrap_or("");
            // Short HH:MM:SS slice if RFC3339; otherwise full ts.
            let short_ts = ts.get(11..19).unwrap_or(ts);
            let summary = crate::cmd::render::summarize_payload(kind, &e["payload"]);
            out.push(format!("  {short_ts}  {kind:<14}  {summary}"));
        }
        out.join("\n")
    }
}

pub fn exec(db_path: &std::path::Path, a: ShowArgs) -> Result<ShowView> {
    let conn = db::open(db_path)?;
    let resolved = id::resolve_full(&conn, &a.task)?;
    let tid = resolved.id;
    let display_id = resolved.display_id;

    // Core task record.
    let (title, state, tier, description, created_at): (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = conn.query_row(
        "SELECT title, state, tier, description, created_at FROM task WHERE id = ?1",
        [tid],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;

    let agent = store::latest_agent(&conn, tid)?;
    // The open hold, if there is one, and whether it has gone quiet past its
    // lease. Evaluated here, on read, because nothing in quipu runs on a timer.
    let hold = lease::hold_of(&conn, tid)?;

    let mut tag_stmt = conn.prepare("SELECT name FROM tag WHERE task_id = ?1 ORDER BY name")?;
    let mut tags: Vec<String> = tag_stmt
        .query_map([tid], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    tags.sort();

    // Three lists, not one. All three are `dep` rows, and containment keeps a
    // container out of `ready` exactly as a blocker does — but rendering a wave
    // as "blocked by" the slices it is made of describes the graph wrongly.
    // `blocked_by` is what stops this ticket, `contains` is what it is made of,
    // `part_of` is what it belongs to.
    let blocked_by = store::unresolved_blockers_by_task(&conn, &[tid])?
        .remove(&tid)
        .unwrap_or_default();
    let contains = store::contents_by_task(&conn, &[tid])?
        .remove(&tid)
        .unwrap_or_default();
    let part_of = store::containers_of(&conn, tid)?;
    // Why a `pending` task with no blockers of its own is still not workable.
    // Without this the ticket shows `pending`, an empty `blocked_by`, and no
    // way to find the prerequisite that is actually holding it.
    let frozen_by = store::frozen_by(&conn, tid)?;

    // Every event, newest first. The query is deliberately unbounded: `--json`
    // emits all of them (see `report --ticket` for the same contract), and the
    // human tail is trimmed below where the total is still known, so the
    // rendering can say how many it dropped.
    let mut s = conn.prepare(
        "SELECT ts, kind, agent_id, payload FROM event
          WHERE task_id = ?1 ORDER BY id DESC",
    )?;
    let events: Vec<(String, String, Option<String>, Value)> = s
        .query_map([tid], |r| {
            let payload: Option<String> = r.get(3)?;
            let payload_v: Value = payload
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .ok()
                .flatten()
                .unwrap_or(Value::Null);
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                payload_v,
            ))
        })?
        .collect::<Result<_, _>>()?;

    // last_event is the newest (events[0] if present).
    let last_event = events.first().map(|(ts, kind, _agent, payload)| {
        serde_json::json!({
            "kind": kind, "ts": ts, "payload": payload.clone(),
        })
    });
    // recent_events ordered oldest-first for friendlier consumption.
    let recent_events: Vec<Value> = events
        .iter()
        .rev()
        .map(|(ts, kind, agent, payload)| {
            serde_json::json!({
                "ts": ts, "kind": kind, "agent_id": agent, "payload": payload.clone(),
            })
        })
        .collect();
    let mut obj = serde_json::json!({
        "id": tid,
        "display_id": display_id,
        "title": title,
        "state": state,
        "tier": tier,
        "agent": agent,
        "tags": tags,
        "blocked_by": blocked_by,
        "contains": contains,
        "part_of": part_of,
        "frozen_by": frozen_by.iter().map(|(container, blocker)| {
            serde_json::json!({"container": container, "blocker": blocker})
        }).collect::<Vec<_>>(),
        "last_event": last_event,
        "recent_events": recent_events,
        "hold": hold.as_ref().map(lease::Hold::to_json),
    });
    if let Some(d) = description.as_ref() {
        obj.as_object_mut()
            .unwrap()
            .insert("description".into(), Value::String(d.clone()));
    }
    if let Some(c) = created_at.as_ref() {
        obj.as_object_mut()
            .unwrap()
            .insert("created_at".into(), Value::String(c.clone()));
    }
    Ok(ShowView(obj))
}

/// Hard-wrap text to `width` columns, splitting on whitespace. Preserves
/// existing newline boundaries.
pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if line.is_empty() {
                line.push_str(word);
            } else if line.len() + 1 + word.len() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                line.push_str(word);
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}
