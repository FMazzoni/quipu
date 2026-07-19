//! Display-id encoding for human-friendly task references.
//!
#![doc = include_str!("../docs/modules/id.md")]

use anyhow::{anyhow, Result};

/// A resolved task: its rowid plus the store's canonical `display_id` for it.
///
/// Callers that print back to the user should use `display_id`, not the raw
/// argument they were given — the argument may be lowercase, zero-padded, or
/// padded with whitespace, none of which the user wants echoed back.
pub struct Resolved {
    pub id: i64,
    pub display_id: String,
}

pub fn encode(rowid: i64, prefix: &str) -> String {
    format!("{prefix}-{rowid}")
}

pub fn parse(s: &str) -> Result<i64> {
    let s = s.trim();
    // New form: <LETTERS>-<DIGITS>
    if let Some((head, tail)) = s.split_once('-') {
        if !head.is_empty()
            && head.bytes().all(|b| b.is_ascii_alphabetic())
            && !tail.is_empty()
            && tail.bytes().all(|b| b.is_ascii_digit())
        {
            let n: i64 = tail.parse().map_err(|_| anyhow!("invalid id `{s}`"))?;
            if n <= 0 {
                return Err(anyhow!("invalid id `{s}`"));
            }
            return Ok(n);
        }
    }
    // Legacy form: T<DIGITS> (case-insensitive). Kept for one release.
    if let Some(rest) = s.strip_prefix('T').or_else(|| s.strip_prefix('t')) {
        if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
            let n: i64 = rest.parse().map_err(|_| anyhow!("invalid id `{s}`"))?;
            if n <= 0 {
                return Err(anyhow!("invalid id `{s}`"));
            }
            return Ok(n);
        }
    }
    Err(anyhow!("invalid id `{s}` (expected <PREFIX>-<n>)"))
}

/// Resolve a user-supplied task reference to its rowid.
///
/// Prefer `resolve_full` when the canonical `display_id` is also needed (e.g.
/// to echo it back to the user) — it's the same query, just fetching one more
/// column.
pub fn resolve(conn: &rusqlite::Connection, s: &str) -> Result<i64> {
    Ok(resolve_full(conn, s)?.id)
}

/// Resolve a task reference to both its rowid and its canonical `display_id`.
///
/// Matches numerically on the parsed rowid, so `QP-1`, `qp-001`, and (for one
/// more release) `T1` all resolve to the same row regardless of case,
/// zero-padding, or the store's actual prefix.
pub fn resolve_full(conn: &rusqlite::Connection, s: &str) -> Result<Resolved> {
    let n = parse(s)?;
    conn.query_row("SELECT id, display_id FROM task WHERE id = ?", [n], |r| {
        Ok(Resolved {
            id: r.get(0)?,
            display_id: r.get(1)?,
        })
    })
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => {
            crate::db::not_found(format!("no such task: {s}"), Some(s.trim().to_string()))
        }
        other => other.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_uses_supplied_prefix() {
        assert_eq!(encode(1, "QP"), "QP-1");
        assert_eq!(encode(42, "ACME"), "ACME-42");
    }

    #[test]
    fn parse_accepts_new_form() {
        assert_eq!(parse("QP-1").unwrap(), 1);
        assert_eq!(parse("ACME-42").unwrap(), 42);
        assert_eq!(parse("  qp-7  ").unwrap(), 7);
    }

    #[test]
    fn parse_accepts_legacy_t_form() {
        assert_eq!(parse("T7").unwrap(), 7);
        assert_eq!(parse("t99").unwrap(), 99);
    }

    #[test]
    fn parse_rejects_bad_input() {
        for bad in [
            "", "7", "-7", "QP-", "QP-0", "QP-abc", "1-QP", "QP--1", "Q1-1",
        ] {
            assert!(parse(bad).is_err(), "should reject `{bad}`");
        }
    }

    #[test]
    fn resolve_handles_whitespace_and_lowercase() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let conn = crate::db::open(&db).unwrap();
        conn.execute(
            "INSERT INTO task(display_id, title) VALUES ('QP-1', 'first')",
            [],
        )
        .unwrap();
        let rowid = conn.last_insert_rowid();
        assert_eq!(resolve(&conn, "QP-1").unwrap(), rowid);
        assert_eq!(resolve(&conn, "qp-1").unwrap(), rowid);
        assert_eq!(resolve(&conn, "  qp-1  ").unwrap(), rowid);
    }

    #[test]
    fn resolve_handles_zero_padding() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let conn = crate::db::open(&db).unwrap();
        conn.execute(
            "INSERT INTO task(display_id, title) VALUES ('QP-1', 'first')",
            [],
        )
        .unwrap();
        let rowid = conn.last_insert_rowid();
        assert_eq!(resolve(&conn, "QP-001").unwrap(), rowid);
        assert_eq!(resolve(&conn, "qp-0001").unwrap(), rowid);
    }

    #[test]
    fn resolve_handles_legacy_t_form() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let conn = crate::db::open(&db).unwrap();
        conn.execute(
            "INSERT INTO task(display_id, title) VALUES ('T1', 'first')",
            [],
        )
        .unwrap();
        let rowid = conn.last_insert_rowid();
        assert_eq!(resolve(&conn, "T1").unwrap(), rowid);
        assert_eq!(resolve(&conn, "t1").unwrap(), rowid);
    }

    #[test]
    fn resolve_full_returns_canonical_display_id() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let conn = crate::db::open(&db).unwrap();
        conn.execute(
            "INSERT INTO task(display_id, title) VALUES ('QP-1', 'first')",
            [],
        )
        .unwrap();
        let rowid = conn.last_insert_rowid();
        let r = resolve_full(&conn, "  qp-001  ").unwrap();
        assert_eq!(r.id, rowid);
        assert_eq!(r.display_id, "QP-1");
    }

    #[test]
    fn resolve_missing_task_echoes_raw_input() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let conn = crate::db::open(&db).unwrap();
        let err = resolve(&conn, "QP-999").unwrap_err();
        assert_eq!(err.to_string(), "no such task: QP-999");
    }
}
