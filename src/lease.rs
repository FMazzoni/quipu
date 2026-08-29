//! Claim liveness: how long a held ticket may sit quiet before its hold is stale.
//!
#![doc = include_str!("../docs/modules/lease.md")]

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction};
use std::collections::HashMap;

/// The lease length a claim takes when the operator states none.
///
/// Eight hours, measured against real usage rather than picked for looking
/// round: the quiet gaps a live holder actually leaves between writes run
/// hours, not minutes, and a tighter default breaks live leases — which
/// reintroduces the two-holders bug the lease exists to prevent. It is spelled
/// as a suffixed string so the default goes through the same parser as an
/// operator-supplied one and cannot drift from it.
pub const DEFAULT_TTL: &str = "8h";

/// Environment override for the machine name recorded on a claim.
///
/// Exists so a test can pin the value; a real claim reads the host name.
pub const HOST_ENV: &str = "QP_HOST";

/// Parses a lease length. A unit suffix is required: `30s`, `45m`, `8h`, `2d`.
///
/// The suffix is not sugar. A bare number is ambiguous in exactly the way that
/// sets a lease to seven *seconds* when seven *hours* was meant, so it is
/// rejected rather than guessed at.
pub fn parse_ttl(s: &str) -> Result<i64> {
    let t = s.trim();
    let bad = || {
        crate::db::invalid_input(format!(
            "`{t}` is not a lease length: a unit suffix is required (30s, 45m, 8h, 2d). \
             A bare number is ambiguous, and guessing at it is how a lease meant for \
             hours ends up lasting seconds."
        ))
    };
    let (digits, unit) = t.split_at(t.len().saturating_sub(1));
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(bad()),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: i64 = digits.parse().map_err(|_| bad())?;
    if n == 0 {
        return Err(crate::db::invalid_input(format!(
            "`{t}` is a zero-length lease, which would be stale the instant it was taken"
        )));
    }
    Ok(n * mult)
}

/// This machine's name, for `assignment.holder_host`.
///
/// `None` rather than a placeholder when the platform will not say: a row that
/// admits it does not know which machine holds it is honest, and one that
/// invents `unknown` as a host name is not.
pub fn host_id() -> Option<String> {
    if let Ok(v) = std::env::var(HOST_ENV) {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    platform_host()
}

#[cfg(unix)]
fn platform_host() -> Option<String> {
    let mut buf = [0 as libc::c_char; 256];
    // SAFETY: `gethostname` writes at most `len` bytes into `buf` and NUL
    // terminates within it; the buffer outlives the call.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len() - 1) };
    if rc != 0 {
        return None;
    }
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    let s = String::from_utf8_lossy(&bytes).trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(not(unix))]
fn platform_host() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// SQL: is the open assignment aliased `a` still a live hold?
///
/// The single definition of the predicate. Both the read path and `reclaim`'s
/// guarded `UPDATE` embed this string, so "what `qp show` calls stale" and
/// "what `reclaim` will break" cannot drift into two different questions.
///
/// A row with no `lease_seconds` or no `last_contact` is live forever: it never
/// stated an expiry, and inventing one for it would break a claim that was
/// promised nothing of the sort. A `last_contact` SQLite cannot parse is
/// treated the same way, for the same reason: an unreadable stamp is not
/// evidence that a holder is gone.
///
/// Both `CAST`s are load-bearing. `strftime` returns TEXT, and SQLite sorts
/// every INTEGER below every TEXT, so the uncast comparison is false for every
/// row on earth — which reads as "everything is stale" and hands `reclaim`
/// every live hold in the store.
pub const LIVE_HOLD: &str = "(a.claimed_at IS NOT NULL AND (\
       a.lease_seconds IS NULL OR a.last_contact IS NULL \
       OR strftime('%s', a.last_contact) IS NULL \
       OR CAST(strftime('%s', a.last_contact) AS INTEGER) + a.lease_seconds \
          > CAST(strftime('%s','now') AS INTEGER)))";

/// SQL: the instant the hold aliased `a` expires, or NULL if it never does.
const EXPIRES_AT: &str = "(CASE WHEN a.lease_seconds IS NULL OR a.last_contact IS NULL THEN NULL \
       ELSE strftime('%Y-%m-%dT%H:%M:%SZ', a.last_contact, '+' || a.lease_seconds || ' seconds') \
       END)";

/// What one open, claimed assignment says about its holder's liveness.
pub struct Hold {
    pub agent_id: String,
    pub host: Option<String>,
    pub last_contact: Option<String>,
    pub lease_seconds: Option<i64>,
    pub expires_at: Option<String>,
    pub stale: bool,
}

impl Hold {
    /// The read-command rendering, shared by `show` and `wave`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "agent": self.agent_id,
            "host": self.host,
            "last_contact": self.last_contact,
            "lease_seconds": self.lease_seconds,
            "expires_at": self.expires_at,
            "stale": self.stale,
        })
    }
}

/// The human marker a stale hold trails behind a row's last column.
///
/// One spelling, shared by `list` and `wave`, so two views of the same
/// stranded ticket cannot describe it differently. It *trails* rather than
/// taking a column of its own: `list`'s column set is pinned to `show`'s
/// header line by `show_header_mirrors_list_columns_and_tier_is_labelled`, so
/// a fifth column is a positional change every reader of both views inherits,
/// and the marker earns a sentence rather than that.
///
/// Takes the `hold` object [`Hold::to_json`] emits, which carries `stale`
/// itself — reading liveness out of the same object that names the holder is
/// what keeps a caller from pairing this text with some other row's flag. A
/// missing or `null` hold renders nothing.
pub fn stale_marker(hold: &serde_json::Value) -> String {
    if !hold["stale"].as_bool().unwrap_or(false) {
        return String::new();
    }
    format!(
        "   [STALE — quiet since {}, on {}]",
        hold["last_contact"].as_str().unwrap_or("?"),
        hold["host"].as_str().unwrap_or("unknown host")
    )
}

const HOLD_COLUMNS: &str = "a.task_id, a.agent_id, a.holder_host, a.last_contact, a.lease_seconds";

fn hold_from_row(r: &rusqlite::Row) -> rusqlite::Result<(i64, Hold)> {
    Ok((
        r.get(0)?,
        Hold {
            agent_id: r.get(1)?,
            host: r.get(2)?,
            last_contact: r.get(3)?,
            lease_seconds: r.get(4)?,
            expires_at: r.get(5)?,
            stale: r.get::<_, i64>(6)? == 0,
        },
    ))
}

/// The open, claimed hold on one task, if it has one.
pub fn hold_of(conn: &Connection, task_id: i64) -> Result<Option<Hold>> {
    let sql = format!(
        "SELECT {HOLD_COLUMNS}, {EXPIRES_AT}, {LIVE_HOLD} FROM assignment a
          WHERE a.task_id = ?1 AND a.completed_at IS NULL AND a.claimed_at IS NOT NULL
          ORDER BY a.id DESC LIMIT 1"
    );
    Ok(conn
        .query_row(&sql, [task_id], hold_from_row)
        .optional()?
        .map(|(_, h)| h))
}

/// The open, claimed holds on a batch of tasks, keyed by task id.
///
/// One query for the whole page: a read command renders many rows and must not
/// issue a statement per row to decide which of them are stranded.
pub fn holds_by_task(conn: &Connection, ids: &[i64]) -> Result<HashMap<i64, Hold>> {
    let mut out = HashMap::new();
    // Chunked like the bulk helpers in `store`, and for the same reason: an
    // `IN (...)` list is bounded by SQLite's variable limit.
    for chunk in ids.chunks(32_000) {
        if chunk.is_empty() {
            continue;
        }
        let places = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {HOLD_COLUMNS}, {EXPIRES_AT}, {LIVE_HOLD} FROM assignment a
              WHERE a.completed_at IS NULL AND a.claimed_at IS NOT NULL
                AND a.task_id IN ({places})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let pref: Vec<&dyn rusqlite::ToSql> =
            chunk.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
        for row in stmt.query_map(pref.as_slice(), hold_from_row)? {
            let (id, hold) = row?;
            out.insert(id, hold);
        }
    }
    Ok(out)
}

/// Stamp a fresh lease onto the assignment row `claim` just claimed.
pub fn take(tx: &Transaction, assignment_id: i64, seconds: i64) -> Result<()> {
    tx.execute(
        &format!(
            "UPDATE assignment SET last_contact = {NOW}, lease_seconds = ?1, holder_host = ?2
              WHERE id = ?3"
        ),
        rusqlite::params![seconds, host_id(), assignment_id],
    )?;
    Ok(())
}

/// SQL: the stamp written into `last_contact`, in the format the rest of the
/// schema uses.
///
/// Not `time::now_rfc3339`, and the difference is not cosmetic: that formats
/// with nanosecond precision, which is nine fractional digits where SQLite's
/// date functions accept three. A stamp SQLite cannot read is a lease nothing
/// can evaluate.
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// Move a hold's last-contact forward, or reject a holder that has lost it.
///
/// Called from `db::insert_event`, which is the one place every write against a
/// task passes through — renewal spread across the ten commands that can write
/// against a held ticket is renewal that a new command silently forgets.
///
/// This is what makes the TTL bound *quiet* time rather than work time: a
/// holder that is still writing keeps pushing its own expiry out, so a ticket
/// that takes three days but is logged against hourly never goes stale.
pub fn touch(tx: &Transaction, task_id: i64, agent: Option<&str>) -> Result<()> {
    let n = tx.execute(
        &format!(
            "UPDATE assignment SET last_contact = {NOW}
              WHERE task_id = ?1 AND completed_at IS NULL AND claimed_at IS NOT NULL
                AND lease_seconds IS NOT NULL AND (?2 IS NULL OR agent_id = ?2)"
        ),
        rusqlite::params![task_id, agent],
    )?;
    if n > 0 {
        return Ok(());
    }
    let Some(agent) = agent else { return Ok(()) };
    if let Some(e) = dispossessed(tx, task_id, agent)? {
        return Err(e);
    }
    Ok(())
}

/// The error a holder gets on its first write after its lease was broken.
///
/// `None` when this agent was not dispossessed — it never held the task, still
/// holds it, or released it itself. The narrow case is the one the whole design
/// turns on: a machine that comes back to find its ticket taken must be told
/// so, by name, and must then proceed as a store that holds nothing rather than
/// wedging the ticket forever.
pub fn dispossessed(tx: &Transaction, task_id: i64, agent: &str) -> Result<Option<anyhow::Error>> {
    let latest: Option<(Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT outcome, completed_at FROM assignment
              WHERE task_id = ?1 AND agent_id = ?2 ORDER BY id DESC LIMIT 1",
            rusqlite::params![task_id, agent],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((Some(outcome), Some(when))) = latest else {
        return Ok(None);
    };
    if outcome != "reclaimed" {
        return Ok(None);
    }
    let display: String =
        tx.query_row("SELECT display_id FROM task WHERE id = ?", [task_id], |r| {
            r.get(0)
        })?;
    let holder: Option<String> = tx
        .query_row(
            "SELECT agent_id FROM assignment
              WHERE task_id = ?1 AND completed_at IS NULL ORDER BY id DESC LIMIT 1",
            [task_id],
            |r| r.get(0),
        )
        .optional()?;
    let now_held = match holder.as_deref() {
        Some(h) => format!("; `{h}` holds it now"),
        None => String::new(),
    };
    Ok(Some(crate::db::not_owner(
        format!(
            "`{agent}` no longer holds {display}: the hold was reclaimed at {when}{now_held}. \
             This store holds nothing on {display} — do not retry, and do not keep writing \
             against it. Ask for a fresh assignment if the work still needs doing."
        ),
        Some(display),
        holder,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_needs_a_unit_suffix() {
        assert_eq!(parse_ttl("30s").unwrap(), 30);
        assert_eq!(parse_ttl("45m").unwrap(), 2_700);
        assert_eq!(parse_ttl("8h").unwrap(), 28_800);
        assert_eq!(parse_ttl("2d").unwrap(), 172_800);
        // The whole point: a bare number is refused, not guessed at.
        assert!(parse_ttl("8").is_err());
        assert!(parse_ttl("").is_err());
        assert!(parse_ttl("h").is_err());
        assert!(parse_ttl("8hours").is_err());
        assert!(parse_ttl("-1h").is_err());
        assert!(parse_ttl("0h").is_err());
    }

    #[test]
    fn the_default_ttl_parses_and_is_eight_hours() {
        assert_eq!(parse_ttl(DEFAULT_TTL).unwrap(), 8 * 3_600);
    }
}
