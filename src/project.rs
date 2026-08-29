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

/// The store's own uuid: the identity minted at `qp init` and never reused.
///
/// Lives here rather than beside the remote config because it is identity, not
/// attachment — every store has one whether or not it is attached to anything,
/// and it is what [`canonical_id`] and [`legacy_key`] are built out of.
pub fn store_uuid(conn: &Connection) -> Result<String> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'project_uuid'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    v.ok_or_else(|| crate::db::invalid_input("store has no project_uuid; run `qp init` first"))
}

/// The `(origin_uuid, origin_task_id)` pair naming `task_id` anywhere in the world.
///
/// A row in `task_origin` means the task was minted elsewhere and carries that
/// store's name for it. No row means it was minted here, and its canonical name
/// is this store's uuid plus its own rowid — which is why `qp add` writes
/// nothing to that table and never has to.
///
/// GOTCHA — this is the only cross-store identity computation in the tree, and
/// at the moment it has no caller: it arrived with the git claim authority,
/// which is gone, and its next caller is whatever teaches the server to name a
/// ticket the same way in two stores. It is kept rather than deleted because
/// the rule it encodes — peers are not replicas, so a name derived from the
/// *asking* store's uuid or from a display id makes two peers compute two
/// different names for one ticket — is not recoverable from the schema, and
/// rewriting it from scratch is how that rule gets broken.
#[allow(dead_code)]
pub fn canonical_id(conn: &Connection, task_id: i64) -> Result<(String, i64)> {
    let found: Option<(String, i64)> = conn
        .query_row(
            "SELECT origin_uuid, origin_task_id FROM task_origin WHERE task_id = ?1",
            [task_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match found {
        Some(pair) => Ok(pair),
        None => Ok((store_uuid(conn)?, task_id)),
    }
}

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

/// `meta` key holding the human name this store answers to.
///
/// Distinct from [`KEY_PROJECT`] on purpose: the key is a uuid that identifies a
/// project across machines, the name is what a person (or a `--project` flag)
/// types. Neither can do the other's job — a uuid is not typeable, and a name is
/// only unique within one machine's registry.
pub const KEY_NAME: &str = "project_name";

/// `meta` key recording whether the name was chosen or derived.
///
/// The whole reason a derived name is allowed to exist at all: a store that
/// predates naming must not be bricked, but the placeholder it is healed with
/// must not then be frozen as if a person had picked it. `derived` may be
/// replaced silently; `assigned` may not be replaced at all.
pub const KEY_NAME_ORIGIN: &str = "project_name_origin";

/// A name a person typed (or that `qp init` took from the directory).
pub const ORIGIN_ASSIGNED: &str = "assigned";

/// A placeholder minted for a store that existed before names did.
pub const ORIGIN_DERIVED: &str = "derived";

/// Longest name accepted.
///
/// Long enough for a readable slug, short enough to stay typeable as a
/// `--project` argument and to fit a listing column.
pub const NAME_MAX: usize = 64;

/// Rejects a name that cannot be typed unambiguously.
///
/// Lowercase ASCII letters, digits and hyphens, starting and ending
/// alphanumeric. The character set is not aesthetic: a name is a shell argument
/// and a lookup key, so anything that shells quote, that normalises two ways
/// (case, Unicode) or that reads as a flag would make two spellings of one
/// project — and a `--project` that silently matches nothing is the exact
/// failure the name exists to prevent. Uppercase is rejected rather than folded
/// so the stored name and the typed name are the same string.
pub fn validate_name(name: &str) -> Result<()> {
    let bad = |why: &str| crate::db::invalid_input(format!("project name `{name}` {why}"));
    if name.is_empty() {
        return Err(bad("is empty; names are 1-64 characters of [a-z0-9-]"));
    }
    if name.len() > NAME_MAX {
        return Err(bad(&format!("is longer than {NAME_MAX} characters")));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(bad(
            "may contain only lowercase letters, digits and hyphens",
        ));
    }
    let ends_alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    if !ends_alnum(name.as_bytes()[0]) || !ends_alnum(name.as_bytes()[name.len() - 1]) {
        return Err(bad("must start and end with a letter or digit"));
    }
    Ok(())
}

/// Turns arbitrary text into a name [`validate_name`] accepts, or `None`.
///
/// Every run of characters outside the set collapses to one hyphen, which is
/// what makes the mapping stable: two directory names differing only in
/// punctuation must not become two spellings that look the same in a listing.
pub fn slug(raw: &str) -> Option<String> {
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let s = out.trim_matches('-');
    let s: String = s.chars().take(NAME_MAX).collect();
    let s = s.trim_end_matches('-').to_string();
    (!s.is_empty()).then_some(s)
}

/// The placeholder name a store with none is healed with.
///
/// Derived from the store's own durable identity rather than from where the
/// file happens to sit: a name taken from a directory would change the moment
/// the store moved, and a name that drifts is worse than an ugly one. The uuid
/// tail is what keeps two healed stores on one machine from claiming one name.
pub fn derived_name(display_prefix: &str, project_uuid: &str) -> String {
    let head = slug(display_prefix).unwrap_or_else(|| "qp".to_string());
    let tail: String = project_uuid
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if tail.is_empty() {
        head
    } else {
        format!("{head}-{tail}")
    }
}

/// The name this store answers to, if it has one.
pub fn read_name(conn: &Connection) -> Result<Option<String>> {
    let v: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [KEY_NAME], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(v.filter(|s| !s.is_empty()))
}

/// Whether the current name was chosen or merely derived.
pub fn name_origin(conn: &Connection) -> Result<Option<String>> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [KEY_NAME_ORIGIN],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.filter(|s| !s.is_empty()))
}

/// Reads the name, or fails loudly rather than letting a caller guess one.
pub fn require_name(conn: &Connection) -> Result<String> {
    read_name(conn)?.ok_or_else(|| {
        crate::db::invalid_input("store has no project_name; run `qp init --name <NAME>`")
    })
}

/// Gives a store with no name the derived one it is understood to have.
///
/// Ungated by `schema_version`, exactly like [`backfill`] and
/// `db::add_missing_columns`, and for the same reason: a store stamped at a
/// version that already existed before names did would otherwise never be
/// looked at again.
///
/// GOTCHA — do NOT fold these two rows into `migrate`'s
/// `INSERT OR IGNORE INTO meta(...) VALUES (...)` batch. That statement ignores
/// per ROW, not per statement, so a store missing only this key would be handed
/// a freshly minted value there and this function would never see it: the same
/// bug that made [`backfill`]'s derived key unreachable for stores that already
/// existed.
pub fn backfill_name(conn: &Connection) -> Result<()> {
    if read_name(conn)?.is_some() {
        return Ok(());
    }
    let uuid: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'project_uuid'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    // No uuid means no store yet — there is nothing to name.
    let Some(uuid) = uuid else { return Ok(()) };
    let prefix: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'display_prefix'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| "qp".to_string());
    write_name(conn, &derived_name(&prefix, &uuid), ORIGIN_DERIVED)
}

/// Records a name and how it was arrived at, replacing whatever is there.
fn write_name(conn: &Connection, name: &str, origin: &str) -> Result<()> {
    validate_name(name)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![KEY_NAME, name],
    )?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![KEY_NAME_ORIGIN, origin],
    )?;
    Ok(())
}

/// Settles what this store is called, at `qp init` and only there.
///
/// INVARIANT: an assigned name is stable for the life of the store. Registry
/// entries, and one day a `--project` flag in a running server, are keyed by it;
/// letting `qp init` quietly rename a store would re-point every one of them at
/// once and the tickets would still be there, under a name nothing refers to.
/// So a second `--name` that disagrees is refused, not applied — the same call
/// `db::init` already makes for `project_key`.
///
/// A DERIVED name is the one exception, and the reason the origin is recorded:
/// it is a placeholder this binary invented for a store that predates naming,
/// so replacing it is the migration finishing rather than a rename.
pub fn assign_name(
    conn: &Connection,
    db_path: &Path,
    requested: Option<&str>,
    fresh: bool,
) -> Result<String> {
    if let Some(want) = requested {
        validate_name(want)?;
    }
    let have = read_name(conn)?;
    let origin = name_origin(conn)?;
    match (have, requested) {
        (Some(have), Some(want)) if have == want => Ok(have),
        (Some(have), Some(want)) if origin.as_deref() == Some(ORIGIN_ASSIGNED) => {
            Err(crate::db::conflict(
                "project_name_mismatch",
                format!(
                    "this store is named `{have}` and cannot be renamed to `{want}`. A name is \
                     fixed once assigned: registry entries point at it, so a rename here would \
                     leave every one of them naming a project that no longer answers."
                ),
                None,
            ))
        }
        // Either no name at all, or a derived placeholder being settled.
        (_, Some(want)) => {
            write_name(conn, want, ORIGIN_ASSIGNED)?;
            Ok(want.to_string())
        }
        // An existing store keeps what it has. That includes a derived
        // placeholder: `qp init` in a store that already exists is routine, and
        // an unasked-for rename on a routine command is exactly what the
        // invariant above forbids. `qp init --name` is how that one is settled.
        (Some(have), None) if !fresh => Ok(have),
        // A store created by THIS invocation. `backfill_name` has already given
        // it the derived placeholder by now — `db::migrate` runs before anything
        // here — so "no name yet" is not a state this can observe, and freshness
        // has to be established by the caller rather than inferred from the row.
        (_, None) => {
            let default = default_name(db_path, conn)?;
            write_name(conn, &default, ORIGIN_ASSIGNED)?;
            Ok(default)
        }
    }
}

/// The name `qp init` picks when nobody says one.
///
/// The directory holding `.quipu/`, slugged — that is what a person calls the
/// project when asked. Falls back to the derived form when the path yields
/// nothing usable (a store at the filesystem root, a directory named `...`).
fn default_name(db_path: &Path, conn: &Connection) -> Result<String> {
    // A store lives at `<project>/.quipu/db.sqlite` by convention, so the
    // directory worth naming is one above the file — except when `--db` points
    // somewhere flat, where the holding directory is the best guess there is.
    let dir = db_path.parent();
    let dir = match dir {
        Some(d) if d.file_name() == Some(std::ffi::OsStr::new(".quipu")) => d.parent(),
        other => other,
    };
    let from_path = dir
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .and_then(slug);
    if let Some(n) = from_path {
        return Ok(n);
    }
    let uuid: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'project_uuid'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_default();
    let prefix: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'display_prefix'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| "qp".to_string());
    Ok(derived_name(&prefix, &uuid))
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
        let Some(h) = first_line(&path).as_deref().and_then(header) else {
            continue;
        };
        let entry = best.entry(h.key.clone()).or_insert_with(|| {
            (
                i64::MIN,
                LedgerProject {
                    project_key: h.key.clone(),
                    display_prefix: "QP".to_string(),
                    default_tags: Vec::new(),
                },
            )
        });
        if h.last_event_id > entry.0 {
            entry.0 = h.last_event_id;
            if let Some(p) = h.display_prefix {
                entry.1.display_prefix = p;
            }
            entry.1.default_tags = h.default_tags;
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
        let Some(key) = batch_key(&path) else {
            continue;
        };
        out.entry(key).or_default().push(path);
    }
    Ok(out)
}

/// The projects an object ledger holds, by the rule [`scan`] applies to a directory.
///
/// The key layout puts the project key first, so a delimited LIST offers the
/// candidates for free — and a candidate is a LOCATION, which is not evidence.
/// A url naming the bucket root sees the key prefix another store publishes
/// under as a first segment indistinguishable by name from a project, and
/// offering it as one hands the joiner a project with no batches in it, failing
/// later somewhere that says nothing about why. So each candidate is confirmed
/// the same way a directory is: it is a project when a batch under it carries a
/// header this binary can read.
///
/// The candidate segment is what comes back, not the key its header declares:
/// the header answers *whether there is a project here*, and the segment is
/// *where it is* — the prefix every subsequent LIST and GET addresses. The
/// asymmetry is the point of the whole layout, and a ledger url is a vantage
/// point rather than a privileged one: what projects exist is only ever answered
/// relative to a prefix.
///
/// Costs one GET per candidate rather than per batch: every batch opens with its
/// header, so the first key settles it, and the loop continues only past files
/// that turn out not to be batches at all.
pub fn scan_object(client: &crate::s3::Client) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for candidate in client.list_project_keys()? {
        if holds_a_batch(client, &candidate)? {
            out.push(candidate);
        }
    }
    Ok(out)
}

/// Whether any object under one candidate prefix reads as a batch.
fn holds_a_batch(client: &crate::s3::Client, candidate: &str) -> Result<bool> {
    for key in client.list(&crate::s3::project_prefix(candidate))? {
        // A key that is not `<project>/<store>/<batch>.jsonl` is somebody
        // else's file, or a whole extra segment of prefix — either way it is
        // not a batch of this project, and reading it would prove nothing.
        if crate::s3::key_tail(&key).is_none() {
            continue;
        }
        let body = client.get(&key)?;
        let first = body.split(|b| *b == b'\n').next().unwrap_or_default();
        if std::str::from_utf8(first).ok().and_then(header).is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The envelope one batch header carries, once the line is known to be one.
struct Header {
    key: String,
    display_prefix: Option<String>,
    default_tags: Vec<String>,
    last_event_id: i64,
}

/// Reads one line as a batch header, or `None` when it is not one.
///
/// The single answer to both halves of "is this a batch, and whose project is
/// it". Every reader that enumerates a ledger — a directory walk, an object
/// listing — differs only in how it finds bytes to hand to this; the moment one
/// of them decides for itself what counts as a project, two code paths disagree
/// about what the ledger holds, and the cheaper one is always the wrong one.
fn header(raw: &str) -> Option<Header> {
    let v: Value = serde_json::from_str(raw).ok()?;
    if v.get("record").and_then(Value::as_str) != Some("header")
        || v.get("format").and_then(Value::as_str) != Some(crate::cmd::export::FORMAT)
    {
        return None;
    }
    let uuid = v.get("project_uuid").and_then(Value::as_str)?;
    Some(Header {
        key: effective_key(v.get("project_key").and_then(Value::as_str), uuid),
        display_prefix: v
            .get("display_prefix")
            .and_then(Value::as_str)
            .map(str::to_string),
        default_tags: v
            .get("default_tags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        last_event_id: v
            .get("last_event_id")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
    })
}

/// The project one batch file belongs to; `None` when it is not a batch at all.
fn batch_key(path: &Path) -> Option<String> {
    header(first_line(path)?.as_str()).map(|h| h.key)
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
