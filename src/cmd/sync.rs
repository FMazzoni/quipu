//! `qp sync` — the explicit ledger exchange between a store and its object store.
//!
#![doc = include_str!("../../docs/modules/sync.md")]

use crate::cmd::{export, import};
use crate::outcome::{emit, Outcome};
use crate::{db, project, remote, s3};
use anyhow::{Context, Result};
use clap::Args;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Args, Debug)]
pub struct SyncArgs {
    /// Emit JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

/// One exchange: what went out, what came back, and what the merge did with it.
#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub struct SyncOutcome {
    ledger: String,
    /// The key prefix this store read and wrote. Every object it touched is under it.
    prefix: String,
    /// Whether this run wrote to the object store. False on a second sync with nothing new.
    pushed: bool,
    /// The one object key this run PUT, when it published anything.
    put: Option<String>,
    export: export::ExportOutcome,
    /// Object keys that arrived from this project's other stores.
    ///
    /// Only this project's prefix is ever listed, so another project's batches
    /// are not "seen and skipped" — they are never seen at all. That is the
    /// difference the object store bought over a git fetch.
    fetched: Vec<String>,
    /// Absent when nothing new arrived — the merge is skipped rather than replayed for nothing.
    import: Option<import::ImportOutcome>,
}

impl Outcome for SyncOutcome {
    fn human(&self) -> String {
        let mut lines = Vec::new();
        match self.export.event_count {
            0 => lines.push("nothing new to publish".to_string()),
            n => lines.push(format!(
                "published {n} {}",
                if n == 1 { "event" } else { "events" }
            )),
        }
        if self.pushed {
            lines.push(format!("wrote {} to {}", self.prefix, self.ledger));
        }
        match &self.import {
            None => lines.push("nothing new from the ledger".to_string()),
            Some(i) => lines.push(format!(
                "fetched {} peer {} and merged {} {}",
                self.fetched.len(),
                if self.fetched.len() == 1 {
                    "batch"
                } else {
                    "batches"
                },
                i.events_new,
                if i.events_new == 1 { "event" } else { "events" }
            )),
        }
        lines.join("\n")
    }
}

/// The exchange, in the order the steps have to happen in.
///
/// LIST comes first and the export second, and that order is load-bearing. The
/// watermark — the highest event id this store has already published — is
/// derived from the keys it owns under its own prefix, so a store whose database
/// was rebuilt from the ledger discovers what it already wrote before deciding
/// what to write, instead of republishing its whole history under new names.
///
/// Nothing is ever overwritten. A batch key names the store that wrote it and
/// the event range it closes over, so two stores can never collide and one store
/// never revisits a key — which is why there is no DELETE here, and no
/// retry-on-rejection either. The git ledger needed both because a push is a
/// compare-and-swap on one shared ref; a PUT to a key nobody else can name is
/// not a race.
pub fn run(db_path: &Path, a: SyncArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let url = configured_ledger(&conn)?;
    let uuid = store_uuid(&conn)?;
    let key = project::require(&conn)?;
    drop(conn);

    let client = s3::Client::open(&url)?;
    let project_prefix = s3::project_prefix(&key);
    let own_prefix = s3::store_prefix(&key, &uuid);

    // Only this project's prefix, ever. This one argument is the whole reason
    // the ledger moved off git: a fetch took the branch whole, so every store
    // ended up carrying every other project's history.
    let listed = client.list(&project_prefix)?;

    // Our own batches are ours by construction — they are already in this
    // store's log, and downloading them back would be work with no effect.
    let conn = db::open(db_path)?;
    let own: Vec<&String> = listed
        .iter()
        .filter(|k| k.starts_with(&own_prefix))
        .collect();
    mark_seen(&conn, own.iter().map(|k| k.as_str()))?;
    let watermark = published_watermark(&own, &uuid);
    let already = seen(&conn)?;
    drop(conn);

    let scratch = s3::Scratch::new("sync")?;
    let outbox = scratch.path().join("out");
    std::fs::create_dir_all(&outbox)?;
    let exported = export::export_since(db_path, &outbox, Some(watermark))?;

    let mut put_key = None;
    if let Some(file) = &exported.file {
        let path = Path::new(file);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("the batch just exported has no filename")?;
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading the batch just exported to {file}"))?;
        let object = s3::object_key(&key, &uuid, name);
        client.put(&object, &bytes)?;
        let conn = db::open(db_path)?;
        mark_seen(&conn, std::iter::once(object.as_str()))?;
        put_key = Some(object);
    }

    let fetched: Vec<String> = listed
        .iter()
        .filter(|k| !k.starts_with(&own_prefix) && !already.contains(*k))
        .filter(|k| s3::key_tail(k).is_some())
        .cloned()
        .collect();

    let imported = if fetched.is_empty() {
        None
    } else {
        let inbox = scratch.path().join("in");
        std::fs::create_dir_all(&inbox)?;
        for k in &fetched {
            s3::place(&inbox, k, &client.get(k)?)?;
        }
        let out = import::import(
            db_path,
            &import::ImportArgs {
                from: inbox,
                restore: false,
                merge: true,
                force: false,
                json: false,
            },
        )?;
        // After the merge commits, never before: a key recorded ahead of the
        // events it names would be a batch this store believes it has read and
        // will never fetch again. The other way round costs one redundant
        // download and nothing else.
        let conn = db::open(db_path)?;
        mark_seen(&conn, fetched.iter().map(String::as_str))?;
        Some(out)
    };

    emit(
        a.json,
        &SyncOutcome {
            ledger: url,
            prefix: project_prefix,
            pushed: put_key.is_some(),
            put: put_key,
            export: exported,
            fetched,
            import: imported,
        },
    )
}

/// Brings a ledger onto this machine as a throwaway directory of batch files.
///
/// The joining path (`qp init --from <url>`) and the guard that reads
/// `$QUIPU_LEDGER` both need to READ a ledger before a store exists, and a url
/// is what someone setting up a new machine actually has.
///
/// `project` scopes the download to one project's prefix; `None` takes
/// everything, which only `qp ledger` — a deliberate global read — ever asks
/// for. The directory it fills is temporary and removes itself: nothing here
/// becomes local state, because a persistent mirror is precisely what this
/// design deleted.
pub fn materialise(client: &s3::Client, project: Option<&str>, into: &Path) -> Result<Vec<String>> {
    let prefix = match project {
        Some(k) => s3::project_prefix(k),
        None => String::new(),
    };
    let mut keys = Vec::new();
    for key in client.list(&prefix)? {
        if s3::key_tail(&key).is_none() {
            continue;
        }
        s3::place(into, &key, &client.get(&key)?)?;
        keys.push(key);
    }
    Ok(keys)
}

// ---------------------------------------------------------------------------
// what this store has already accounted for
// ---------------------------------------------------------------------------

/// Records object keys this store has published or merged. Idempotent.
///
/// See the `ledger_object` note in `schema.sql` for why this state exists at all
/// and why losing a row is safe.
pub fn mark_seen<'a>(conn: &Connection, keys: impl Iterator<Item = &'a str>) -> Result<()> {
    let now = crate::time::now_rfc3339();
    let mut stmt =
        conn.prepare("INSERT OR IGNORE INTO ledger_object(key, seen_at) VALUES (?1, ?2)")?;
    for k in keys {
        stmt.execute(rusqlite::params![k, now])?;
    }
    Ok(())
}

/// Every object key this store has already accounted for.
fn seen(conn: &Connection) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare("SELECT key FROM ledger_object")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The highest event id this store has already published, read off its own keys.
///
/// Derived from the immutable object names rather than from a counter, so there
/// is nothing to fall out of sync with the objects themselves — the property
/// `qp export`'s directory scan had, taken from the only place that still holds
/// it now the local clone is gone.
fn published_watermark(own: &[&String], uuid: &str) -> i64 {
    own.iter()
        .filter_map(|k| s3::key_tail(k))
        .filter_map(|(_, file)| export::batch_range(&file, uuid))
        .map(|(_, hi)| hi)
        .max()
        .unwrap_or(0)
}

/// The ledger url, or the refusal that names how to set one.
pub fn configured_ledger(conn: &Connection) -> Result<String> {
    remote::load(conn)?.ledger.ok_or_else(|| {
        db::invalid_input(
            "this store has no ledger remote; run `qp remote set --ledger <url>` first. \
             Sync never guesses a destination, and the claim authority is not one — it is \
             a separate url and sync must never read or write it.",
        )
    })
}

fn store_uuid(conn: &Connection) -> Result<String> {
    conn.query_row(
        "SELECT value FROM meta WHERE key = 'project_uuid'",
        [],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| db::invalid_input("store has no project_uuid; run `qp init` first"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_watermark_comes_from_this_stores_own_object_keys() {
        let uuid = "store-1";
        let keys: Vec<String> = vec![
            "proj/store-1/store-1-0000000001-0000000009.jsonl".to_string(),
            "proj/store-1/store-1-0000000010-0000000042.jsonl".to_string(),
            // A peer's batch with a much higher range must not move our
            // watermark: it names ids in the peer's log, not in ours.
            "proj/store-2/store-2-0000000001-0000009999.jsonl".to_string(),
        ];
        let own: Vec<&String> = keys.iter().collect();
        assert_eq!(published_watermark(&own, uuid), 42);
        assert_eq!(published_watermark(&[], uuid), 0);
    }

    /// The `Some(import)` arm plus both embedded outcomes.
    ///
    /// The registry's `Default` sample leaves all three empty.
    #[test]
    fn a_two_way_sync_round_trips() {
        let import = import::ImportOutcome {
            mode: "merge".to_string(),
            stores: vec!["peer".to_string()],
            events_new: 3,
            ..Default::default()
        };
        let export = export::ExportOutcome {
            event_count: 3,
            file: Some("batch-1.jsonl".to_string()),
            first_event_id: Some(1),
            last_event_id: Some(3),
            ..Default::default()
        };
        crate::outcome::round_trip::assert_round_trips(&SyncOutcome {
            ledger: "s3://example/ledger".to_string(),
            prefix: "example/".to_string(),
            pushed: true,
            put: Some("example/batch-1.jsonl".to_string()),
            export,
            fetched: vec!["example/peer-1.jsonl".to_string()],
            import: Some(import),
        });
    }
}
