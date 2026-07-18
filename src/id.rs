//! Display-id encoding for human-friendly task references.
//!
//! Format: `<PREFIX>-<rowid>` (JIRA-style), e.g. `QP-1`, `ACME-42`. The prefix is
//! per-store, fixed at `qp init`, default `QP`. `parse` accepts any
//! `<LETTERS>-<DIGITS>` form, plus legacy `T<DIGITS>` for one release of grace.
//! `resolve` queries `task.display_id` by exact match, so the prefix in the
//! input is informational — what matters is that the row exists.

use anyhow::{anyhow, Result};

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

pub fn resolve(conn: &rusqlite::Connection, s: &str) -> Result<i64> {
    let _ = parse(s)?;
    let normed = s.trim().to_uppercase();
    let id: i64 = conn
        .query_row("SELECT id FROM task WHERE display_id = ?", [&normed], |r| {
            r.get(0)
        })
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => anyhow!("no such task: {s}"),
            other => other.into(),
        })?;
    Ok(id)
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
}
