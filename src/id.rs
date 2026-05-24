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
    let id: i64 = conn.query_row(
        "SELECT id FROM task WHERE display_id = ?", [s.to_uppercase()], |r| r.get(0)
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
}
