//! Project identity: the key a set of stores share, distinct from the per-store uuid.
//!
#![doc = include_str!("../docs/modules/project.md")]

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

/// `meta` key holding the project identity.
pub const KEY_PROJECT: &str = "project_key";

/// Prefix marking a key derived from a store that predates project identity.
///
/// Never minted for a new project, so a derived key and a minted one can never
/// collide, and reading one tells you which era the store came from.
const LEGACY_PREFIX: &str = "legacy-";

/// A fresh, independent project identity.
///
/// Deliberately unrelated to the store's `project_uuid`: one names the project,
/// the other names the store, and deriving either from the other would make two
/// machines of one project indistinguishable from two projects again the moment
/// something reached for the wrong one.
pub fn mint() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The identity a store or batch that predates `project_key` is understood to have.
///
/// A pre-identity store is its own project — nothing ever linked it to another
/// one, so claiming it shares an identity with anything would be an invention.
/// Derived from `project_uuid` rather than minted so it is stable: the store's
/// own keyless batches, already written to a ledger, resolve to the same key its
/// future batches will carry, and a re-derivation years later agrees with this
/// one.
pub fn legacy_key(project_uuid: &str) -> String {
    format!("{LEGACY_PREFIX}{project_uuid}")
}

/// Rejects a key that could not survive a round trip through a batch header.
pub fn validate(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(crate::db::invalid_input("project key must not be empty"));
    }
    if key.trim() != key {
        return Err(crate::db::invalid_input(
            "project key has leading or trailing whitespace",
        ));
    }
    if key.chars().any(|c| c.is_control()) {
        return Err(crate::db::invalid_input(
            "project key contains a control character",
        ));
    }
    Ok(())
}

/// Reads the key out of `meta`. `None` only before [`backfill`] has ever run.
pub fn read(conn: &Connection) -> Result<Option<String>> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [KEY_PROJECT],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.filter(|s| !s.is_empty()))
}

/// Reads the key, or fails loudly rather than letting a caller guess one.
pub fn require(conn: &Connection) -> Result<String> {
    read(conn)?.ok_or_else(|| {
        crate::db::invalid_input("store has no project_key; run `qp init` to stamp one")
    })
}

/// Gives a pre-identity store the derived key it is understood to have.
///
/// Ungated by `schema_version` on purpose, exactly like `db::add_missing_columns`:
/// a store stamped at a version that already existed before this key did would
/// otherwise never be looked at again, and every project-scoped read would treat
/// it as keyless forever.
pub fn backfill(conn: &Connection) -> Result<()> {
    if read(conn)?.is_some() {
        return Ok(());
    }
    let uuid: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'project_uuid'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    // No uuid means no store yet — there is nothing to be the project of.
    let Some(uuid) = uuid else { return Ok(()) };
    conn.execute(
        "INSERT OR IGNORE INTO meta(key, value) VALUES (?1, ?2)",
        rusqlite::params![KEY_PROJECT, legacy_key(&uuid)],
    )?;
    Ok(())
}

/// The key a batch header means, keyless headers included.
///
/// Every batch written before project identity existed lacks the field. Such a
/// batch is not "unknown project" and must not be treated as matching whoever
/// asks: it is the store that wrote it, standing alone, under [`legacy_key`].
pub fn effective_key(header_key: Option<&str>, project_uuid: &str) -> String {
    match header_key.filter(|k| !k.is_empty()) {
        Some(k) => k.to_string(),
        None => legacy_key(project_uuid),
    }
}

/// The project a single batch file belongs to, keyless batches included.
///
/// `None` for anything that is not a batch — a ledger clone legitimately holds
/// a `.git` directory, a README, and other people's leftovers, and a reader
/// that treated an unreadable file as "no project" would be answering a
/// different question than the one asked.
pub fn batch_key(path: &Path) -> Option<String> {
    let raw = first_line(path)?;
    let v = serde_json::from_str::<Value>(&raw).ok()?;
    if v.get("record").and_then(Value::as_str) != Some("header")
        || v.get("format").and_then(Value::as_str) != Some(crate::cmd::export::FORMAT)
    {
        return None;
    }
    let uuid = v.get("project_uuid").and_then(Value::as_str)?;
    Some(effective_key(
        v.get("project_key").and_then(Value::as_str),
        uuid,
    ))
}

/// One project as a ledger directory describes it.
pub struct LedgerProject {
    pub project_key: String,
    pub display_prefix: String,
    pub default_tags: Vec<String>,
}

/// The projects a ledger directory holds, newest header per project winning.
///
/// Reads the first line of each batch and nothing more — enumerating projects
/// must stay cheap enough for `qp init` to do it before deciding anything, and
/// the envelope is entirely in the header.
///
/// Unreadable or foreign files are skipped rather than raised: a ledger clone
/// legitimately holds a `.git` directory, a README, and other people's leftovers.
pub fn scan(root: &Path) -> Result<Vec<LedgerProject>> {
    let mut files = Vec::new();
    collect(root, true, &mut files)?;
    files.sort();

    // Keyed by project key; the header with the highest `last_event_id` wins,
    // because prefix and default tags are store state snapshotted at export
    // time rather than events.
    let mut best: BTreeMap<String, (i64, LedgerProject)> = BTreeMap::new();
    for path in files {
        let Some(raw) = first_line(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if v.get("record").and_then(Value::as_str) != Some("header")
            || v.get("format").and_then(Value::as_str) != Some(crate::cmd::export::FORMAT)
        {
            continue;
        }
        let Some(uuid) = v.get("project_uuid").and_then(Value::as_str) else {
            continue;
        };
        let header_key = v.get("project_key").and_then(Value::as_str);
        let key = effective_key(header_key, uuid);
        let last = v
            .get("last_event_id")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let entry = best.entry(key.clone()).or_insert_with(|| {
            (
                i64::MIN,
                LedgerProject {
                    project_key: key.clone(),
                    display_prefix: "QP".to_string(),
                    default_tags: Vec::new(),
                },
            )
        });
        if last > entry.0 {
            entry.0 = last;
            if let Some(p) = v.get("display_prefix").and_then(Value::as_str) {
                entry.1.display_prefix = p.to_string();
            }
            entry.1.default_tags = v
                .get("default_tags")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
        }
    }
    Ok(best.into_values().map(|(_, p)| p).collect())
}

/// Every batch file under `root`, grouped by the project its header declares.
///
/// The grouping is per FILE, never per directory: a store that predates project
/// identity and has since been stamped has both keyless and keyed batches in
/// one directory, and those belong to two different projects — [`legacy_key`]
/// and the minted one. Grouping by directory would silently file the older half
/// under the newer project.
///
/// Shares [`collect`] and [`first_line`] with [`scan`] so there is one notion of
/// what a batch file is and one place that reads a header to decide its project.
pub fn batches_by_project(root: &Path) -> Result<BTreeMap<String, Vec<PathBuf>>> {
    let mut files = Vec::new();
    collect(root, true, &mut files)?;
    files.sort();

    let mut out: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for path in files {
        let Some(raw) = first_line(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if v.get("record").and_then(Value::as_str) != Some("header")
            || v.get("format").and_then(Value::as_str) != Some(crate::cmd::export::FORMAT)
        {
            continue;
        }
        let Some(uuid) = v.get("project_uuid").and_then(Value::as_str) else {
            continue;
        };
        let key = effective_key(v.get("project_key").and_then(Value::as_str), uuid);
        out.entry(key).or_default().push(path);
    }
    Ok(out)
}

/// Gathers `*.jsonl` paths from `dir`, descending one level when `deep`.
fn collect(dir: &Path, deep: bool, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading ledger {}", dir.display()))?;
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            if deep && path.file_name().is_some_and(|n| n != ".git") {
                collect(&path, false, out)?;
            }
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

fn first_line(path: &Path) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(f).read_line(&mut line).ok()?;
    (!line.trim().is_empty()).then(|| line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_key_is_never_a_legacy_one() {
        let k = mint();
        assert!(!k.starts_with(LEGACY_PREFIX));
        assert!(legacy_key(&k).starts_with(LEGACY_PREFIX));
        assert_ne!(mint(), mint(), "two projects must not share a key");
    }

    #[test]
    fn a_keyless_header_resolves_to_the_store_that_wrote_it() {
        assert_eq!(effective_key(None, "u1"), legacy_key("u1"));
        assert_ne!(effective_key(None, "u1"), effective_key(None, "u2"));
        assert_eq!(effective_key(Some("shared"), "u1"), "shared");
        assert_eq!(effective_key(Some("shared"), "u2"), "shared");
    }

    #[test]
    fn scanning_an_empty_directory_finds_no_projects() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(scan(tmp.path()).unwrap().is_empty());
    }
}
