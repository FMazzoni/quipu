//! Display-id encoding, where the prefix is part of the identifier rather than decoration.
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

/// A task reference as the user spelled it: the rowid, and the prefix if one
/// was given.
///
/// `prefix` is `None` only for the legacy `T<DIGITS>` form, which predates
/// per-store prefixes and so cannot be checked against one.
pub struct TaskRef {
    pub prefix: Option<String>,
    pub rowid: i64,
}

pub fn parse(s: &str) -> Result<TaskRef> {
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
            return Ok(TaskRef {
                prefix: Some(head.to_string()),
                rowid: n,
            });
        }
    }
    // Legacy form: T<DIGITS> (case-insensitive). Kept for one release.
    if let Some(rest) = s.strip_prefix('T').or_else(|| s.strip_prefix('t')) {
        if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
            let n: i64 = rest.parse().map_err(|_| anyhow!("invalid id `{s}`"))?;
            if n <= 0 {
                return Err(anyhow!("invalid id `{s}`"));
            }
            return Ok(TaskRef {
                prefix: None,
                rowid: n,
            });
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
/// Case and zero-padding are still forgiven (`QP-1`, `qp-001`, `  qp-1  ` are
/// one id), but a prefix that is not this store's is rejected rather than
/// ignored: an id names a row in the store that minted it, and the same number
/// exists in every other store under a different prefix.
pub fn resolve_full(conn: &rusqlite::Connection, s: &str) -> Result<Resolved> {
    let r = parse(s)?;
    if let Some(given) = &r.prefix {
        let store = crate::db::display_prefix(conn)?;
        if !given.eq_ignore_ascii_case(&store) {
            return Err(crate::db::invalid_input(format!(
                "id `{}` has prefix `{}`, but this store's prefix is `{}`",
                s.trim(),
                given.to_ascii_uppercase(),
                store
            )));
        }
    }
    conn.query_row(
        "SELECT id, display_id FROM task WHERE id = ?",
        [r.rowid],
        |r| {
            Ok(Resolved {
                id: r.get(0)?,
                display_id: r.get(1)?,
            })
        },
    )
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
        let r = parse("QP-1").unwrap();
        assert_eq!((r.prefix.as_deref(), r.rowid), (Some("QP"), 1));
        let r = parse("ACME-42").unwrap();
        assert_eq!((r.prefix.as_deref(), r.rowid), (Some("ACME"), 42));
        let r = parse("  qp-7  ").unwrap();
        assert_eq!((r.prefix.as_deref(), r.rowid), (Some("qp"), 7));
    }

    #[test]
    fn parse_accepts_legacy_t_form() {
        let r = parse("T7").unwrap();
        assert_eq!((r.prefix.as_deref(), r.rowid), (None, 7));
        let r = parse("t99").unwrap();
        assert_eq!((r.prefix.as_deref(), r.rowid), (None, 99));
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
        let conn = crate::db::init(&db, None, &[], None).unwrap();
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
        let conn = crate::db::init(&db, None, &[], None).unwrap();
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
        let conn = crate::db::init(&db, None, &[], None).unwrap();
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
        let conn = crate::db::init(&db, None, &[], None).unwrap();
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
    fn resolve_rejects_foreign_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        // `init`, not `open`: opening no longer creates. See `db::require_store`.
        let conn = crate::db::init(&db, None, &[], None).unwrap();
        conn.execute(
            "INSERT INTO task(display_id, title) VALUES ('QP-1', 'first')",
            [],
        )
        .unwrap();
        for foreign in ["ACME-1", "acme-1", "ZZZZ-1"] {
            let err = resolve(&conn, foreign).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains("QP"), "{msg} should name the store prefix");
            assert!(
                msg.to_ascii_uppercase()
                    .contains(&foreign.split('-').next().unwrap().to_ascii_uppercase()),
                "{msg} should name the supplied prefix"
            );
            assert_eq!(
                err.downcast_ref::<crate::db::QuipuError>()
                    .map(crate::db::QuipuError::exit_code),
                Some(1)
            );
        }
    }

    #[test]
    fn resolve_honors_non_default_store_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let conn = crate::db::init(&db, Some("ACME"), &[], None).unwrap();
        conn.execute(
            "INSERT INTO task(display_id, title) VALUES ('ACME-1', 'first')",
            [],
        )
        .unwrap();
        let rowid = conn.last_insert_rowid();
        assert_eq!(resolve(&conn, "ACME-1").unwrap(), rowid);
        assert_eq!(resolve(&conn, "acme-001").unwrap(), rowid);
        assert!(resolve(&conn, "QP-1").is_err());
    }

    #[test]
    fn resolve_missing_task_echoes_raw_input() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let conn = crate::db::init(&db, None, &[], None).unwrap();
        let err = resolve(&conn, "QP-999").unwrap_err();
        assert_eq!(err.to_string(), "no such task: QP-999");
    }
}
