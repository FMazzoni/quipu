//! T-prefixed display id encoding for human-friendly task references.
//!
//! Internally tasks are addressed by `rowid` (an `i64`). The CLI surface
//! exposes them as `T1`, `T42`, etc. Parsing is case-insensitive
//! (`T7` and `t7` both work) and whitespace-tolerant — `parse` and
//! `resolve` both trim before validating, so shell pasting of ids with
//! stray spaces or tabs resolves cleanly.

use anyhow::{anyhow, Result};

pub fn encode(rowid: i64) -> String { format!("T{rowid}") }

pub fn parse(s: &str) -> Result<i64> {
    let s = s.trim();
    let rest = s.strip_prefix('T').or_else(|| s.strip_prefix('t'))
        .ok_or_else(|| anyhow!("invalid id `{s}` (must start with T)"))?;
    let n: i64 = rest.parse().map_err(|_| anyhow!("invalid id `{s}`"))?;
    if n <= 0 { return Err(anyhow!("invalid id `{s}`")); }
    Ok(n)
}

pub fn resolve(conn: &rusqlite::Connection, s: &str) -> Result<i64> {
    let _ = parse(s)?;
    // Trim before uppercasing — `parse` is whitespace-tolerant, so `resolve`
    // must be too. Without the trim, `"  t7  ".to_uppercase()` becomes
    // `"  T7  "` and fails the `display_id = ?` lookup with a spurious
    // "no such task" error.
    let normed = s.trim().to_uppercase();
    let id: i64 = conn.query_row(
        "SELECT id FROM task WHERE display_id = ?", [&normed], |r| r.get(0)
    ).map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => anyhow!("no such task: {s}"),
        other => other.into(),
    })?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_prefixes_with_t() {
        assert_eq!(encode(1), "T1");
        assert_eq!(encode(42), "T42");
    }

    #[test]
    fn parse_accepts_upper_and_lower() {
        assert_eq!(parse("T7").unwrap(), 7);
        assert_eq!(parse("t7").unwrap(), 7);
        assert_eq!(parse("  T7 ").unwrap(), 7);
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert!(parse("7").is_err());
        assert!(parse("Tfoo").is_err());
        assert!(parse("T0").is_err());
        assert!(parse("T-1").is_err());
    }

    #[test]
    fn resolve_handles_whitespace_and_lowercase() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let conn = crate::db::open(&db).unwrap();
        conn.execute(
            "INSERT INTO task(display_id, title) VALUES ('T1', 'first')",
            [],
        ).unwrap();
        let rowid = conn.last_insert_rowid();
        assert_eq!(resolve(&conn, "T1").unwrap(), rowid);
        assert_eq!(resolve(&conn, "t1").unwrap(), rowid);
        assert_eq!(resolve(&conn, "  t1  ").unwrap(), rowid);
        assert_eq!(resolve(&conn, "  T1\t").unwrap(), rowid);
    }
}
