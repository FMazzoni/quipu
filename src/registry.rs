//! The machine-local index from a project name to the store file that answers to it.
//!
#![doc = include_str!("../docs/modules/registry.md")]

use crate::db;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Overrides where the registry lives. Set it and quipu touches nothing else.
///
/// Exists for the same reason `QP_DB` does — a test, or a second environment on
/// one machine, must be able to run without reaching the real one. Every test in
/// this repository sets it, and a test that writes to a real config directory is
/// a defect.
pub const ENV_REGISTRY: &str = "QUIPU_REGISTRY";

/// The registry format this binary reads and writes.
///
/// Bump it when the meaning of an existing field changes or a field becomes
/// required. Purely additive fields do not need a bump — `serde` fills a missing
/// optional in — but anything that would make an older `qp` misread the file
/// does, because the refusal below is the only thing standing between a stale
/// binary and a mis-routed lookup.
pub const REGISTRY_VERSION: u32 = 1;

/// One registered project.
///
/// Carries enough identity — `project_uuid` — to tell a store that moved from a
/// different store claiming the same name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub project_key: String,
    pub project_uuid: String,
}

/// The on-disk document. Only ever serialised through [`Registry::save`].
#[derive(Debug, Serialize, Deserialize)]
struct Document {
    /// `default` so a file that omits it deserialises to 0 and reaches
    /// [`migrate`]'s named refusal, rather than dying as a generic serde error
    /// about a missing field — the absence of the version IS the diagnosis.
    #[serde(default)]
    registry_version: u32,
    projects: Vec<Entry>,
}

/// A loaded registry, plus where it came from so every message can name it.
#[derive(Debug)]
pub struct Registry {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
}

/// What a registration turned out to be, so the caller can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Registered {
    /// The name was not in the registry.
    Added,
    /// This exact store was already registered under this name.
    Unchanged,
    /// The same store (same uuid) had moved, or was renamed; the entry was repointed.
    Updated,
}

/// Where the registry file lives.
///
/// `$QUIPU_REGISTRY` first, then the XDG config directory (`$XDG_CONFIG_HOME`,
/// falling back to `~/.config`) via `dirs`, which is the platform's answer
/// rather than ours. There is deliberately no search path and no per-repository
/// registry: a second candidate location is a second answer to "where is
/// project X", and the whole point of this file is that there is one.
pub fn path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os(ENV_REGISTRY) {
        let p = PathBuf::from(p);
        if p.as_os_str().is_empty() {
            return Err(db::invalid_input(format!("{ENV_REGISTRY} is set but empty")));
        }
        return Ok(p);
    }
    let base = dirs::config_dir().ok_or_else(|| {
        db::invalid_input(format!(
            "no config directory on this platform; set {ENV_REGISTRY} to a file path"
        ))
    })?;
    Ok(base.join("quipu").join("registry.json"))
}

/// Reads the registry, treating "not there yet" as empty and anything else as a fault.
///
/// A missing file is the state of every machine before the first
/// `qp project register`, so it cannot be an error. A file that exists and does
/// not parse is the opposite: something wrote it, and guessing what it meant is
/// how an index quietly starts pointing at the wrong stores.
pub fn load() -> Result<Registry> {
    load_from(&path()?)
}

pub fn load_from(path: &Path) -> Result<Registry> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Registry {
                path: path.to_path_buf(),
                entries: Vec::new(),
            })
        }
        Err(e) => {
            return Err(anyhow::Error::from(e))
                .with_context(|| format!("reading registry {}", path.display()))
        }
    };
    if raw.trim().is_empty() {
        return Ok(Registry {
            path: path.to_path_buf(),
            entries: Vec::new(),
        });
    }
    let doc: Document = serde_json::from_str(&raw).map_err(|e| {
        db::invalid_input(format!(
            "registry {} is not readable as quipu registry v{REGISTRY_VERSION}: {e}. \
             Fix the file or move it aside; quipu will not guess what it meant.",
            path.display()
        ))
    })?;
    migrate(doc, path)
}

/// Brings a document of any known version up to the current one.
///
/// One version exists, so this is a version check and nothing more — but it is
/// the seam a second version goes through, and having it here is what makes the
/// version field mean something rather than decorate the file. A FUTURE version
/// is refused rather than read on a best-effort basis: an older binary silently
/// dropping a field it does not understand would write the file back without it.
fn migrate(doc: Document, path: &Path) -> Result<Registry> {
    if doc.registry_version > REGISTRY_VERSION {
        return Err(db::invalid_input(format!(
            "registry {} is version {} and this qp reads version {REGISTRY_VERSION}. \
             Upgrade qp rather than letting an older one rewrite the file.",
            path.display(),
            doc.registry_version
        )));
    }
    if doc.registry_version == 0 {
        return Err(db::invalid_input(format!(
            "registry {} has no `registry_version`. Every quipu registry carries one; \
             a file without it was not written by qp.",
            path.display()
        )));
    }
    Ok(Registry {
        path: path.to_path_buf(),
        entries: doc.projects,
    })
}

impl Registry {
    /// The store a name refers to, or `None`.
    pub fn lookup(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Names claimed by more than one entry, with every path claiming each.
    ///
    /// Only a hand edit or a merge of two config directories can produce one —
    /// [`Registry::register`] refuses to write one — which is exactly why it is
    /// worth reporting: nothing in quipu would otherwise notice.
    pub fn duplicates(&self) -> Vec<(String, Vec<PathBuf>)> {
        let mut out: Vec<(String, Vec<PathBuf>)> = Vec::new();
        for e in &self.entries {
            let paths: Vec<PathBuf> = self
                .entries
                .iter()
                .filter(|o| o.name == e.name)
                .map(|o| o.path.clone())
                .collect();
            if paths.len() > 1 && !out.iter().any(|(n, _)| *n == e.name) {
                out.push((e.name.clone(), paths));
            }
        }
        out
    }

    /// Entries whose store file is no longer there.
    ///
    /// A separate fault from a duplicate, and a different remedy: the store was
    /// moved or deleted, and the index has no way to find out on its own.
    pub fn dangling(&self) -> Vec<&Entry> {
        self.entries.iter().filter(|e| !e.path.exists()).collect()
    }

    /// Claims a name for a store, or refuses and names both claimants.
    ///
    /// The one case that is not a collision is the same store arriving again:
    /// matched by `project_uuid`, not by path, so a store that moved re-points
    /// its entry instead of stranding it. A DIFFERENT store under a taken name
    /// is refused outright — silently re-pointing it would send every later
    /// lookup to the wrong tickets, and a wrong project must fail loudly.
    pub fn register(&mut self, entry: Entry) -> Result<Registered> {
        crate::project::validate_name(&entry.name)?;
        if let Some(existing) = self.lookup(&entry.name) {
            if existing.project_uuid != entry.project_uuid {
                return Err(db::conflict(
                    "project_name_taken",
                    format!(
                        "the name `{}` is already registered to a different store.\n  \
                         registered: {}\n  this store: {}\n\
                         Rename one of them at `qp init --name`, or drop the stale entry with \
                         `qp project unregister {}`.",
                        entry.name,
                        existing.path.display(),
                        entry.path.display(),
                        entry.name
                    ),
                    None,
                ));
            }
            if *existing == entry {
                return Ok(Registered::Unchanged);
            }
        }
        // One store holds one name: an entry for this store under any other
        // name is stale the moment this one is written, and leaving it behind
        // is how a dangling-looking duplicate appears later.
        let had_other = self
            .entries
            .iter()
            .any(|e| e.project_uuid == entry.project_uuid && e.name != entry.name);
        let existed = self.lookup(&entry.name).is_some();
        self.entries
            .retain(|e| e.project_uuid != entry.project_uuid && e.name != entry.name);
        self.entries.push(entry);
        self.entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(if existed || had_other {
            Registered::Updated
        } else {
            Registered::Added
        })
    }

    /// Drops a name, or reports that there was nothing under it.
    pub fn unregister(&mut self, name: &str) -> Result<Entry> {
        let idx = self
            .entries
            .iter()
            .position(|e| e.name == name)
            .ok_or_else(|| {
                db::not_found(
                    format!(
                        "no project named `{name}` in {}. `qp project list` shows what is there.",
                        self.path.display()
                    ),
                    None,
                )
            })?;
        Ok(self.entries.remove(idx))
    }

    /// Writes the registry back, atomically.
    ///
    /// Temp file plus rename, in the registry's own directory so the rename
    /// stays within one filesystem. A half-written index is worse than a stale
    /// one: the stale one still resolves.
    pub fn save(&self) -> Result<()> {
        let doc = Document {
            registry_version: REGISTRY_VERSION,
            projects: self.entries.clone(),
        };
        let body = serde_json::to_string_pretty(&doc)? + "\n";
        let dir = self.path.parent().ok_or_else(|| {
            db::invalid_input(format!(
                "registry path {} has no parent directory",
                self.path.display()
            ))
        })?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating registry directory {}", dir.display()))?;
        let tmp = self.path.with_extension(format!("json.tmp{}", std::process::id()));
        std::fs::write(&tmp, body)
            .with_context(|| format!("writing registry {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("installing registry {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, path: &str, uuid: &str) -> Entry {
        Entry {
            name: name.to_string(),
            path: PathBuf::from(path),
            project_key: "k".to_string(),
            project_uuid: uuid.to_string(),
        }
    }

    fn empty(dir: &Path) -> Registry {
        Registry {
            path: dir.join("registry.json"),
            entries: Vec::new(),
        }
    }

    #[test]
    fn a_second_store_may_not_take_a_claimed_name() {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = empty(tmp.path());
        r.register(entry("alpha", "/one/db.sqlite", "u1")).unwrap();
        let err = r
            .register(entry("alpha", "/two/db.sqlite", "u2"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("/one/db.sqlite"), "{err}");
        assert!(err.contains("/two/db.sqlite"), "{err}");
    }

    #[test]
    fn the_same_store_moving_repoints_rather_than_colliding() {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = empty(tmp.path());
        assert_eq!(
            r.register(entry("alpha", "/one/db.sqlite", "u1")).unwrap(),
            Registered::Added
        );
        assert_eq!(
            r.register(entry("alpha", "/one/db.sqlite", "u1")).unwrap(),
            Registered::Unchanged
        );
        assert_eq!(
            r.register(entry("alpha", "/moved/db.sqlite", "u1")).unwrap(),
            Registered::Updated
        );
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.lookup("alpha").unwrap().path, PathBuf::from("/moved/db.sqlite"));
    }

    #[test]
    fn one_store_holds_one_name() {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = empty(tmp.path());
        r.register(entry("alpha", "/one/db.sqlite", "u1")).unwrap();
        r.register(entry("beta", "/one/db.sqlite", "u1")).unwrap();
        assert_eq!(r.entries.len(), 1, "the old name must not linger");
        assert!(r.lookup("alpha").is_none());
    }

    #[test]
    fn an_unversioned_or_future_file_is_refused_rather_than_read() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("registry.json");
        std::fs::write(&p, r#"{"projects":[]}"#).unwrap();
        let err = load_from(&p).unwrap_err().to_string();
        assert!(err.contains("registry_version"), "{err}");

        std::fs::write(&p, r#"{"registry_version":99,"projects":[]}"#).unwrap();
        let err = load_from(&p).unwrap_err().to_string();
        assert!(err.contains("Upgrade qp"), "{err}");
    }

    #[test]
    fn a_missing_registry_is_empty_and_a_saved_one_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("nested").join("registry.json");
        let mut r = load_from(&p).unwrap();
        assert!(r.entries.is_empty());
        r.register(entry("alpha", "/one/db.sqlite", "u1")).unwrap();
        r.save().unwrap();
        let back = load_from(&p).unwrap();
        assert_eq!(back.entries, r.entries);
    }

    #[test]
    fn a_hand_edited_duplicate_is_reportable() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("registry.json");
        std::fs::write(
            &p,
            r#"{"registry_version":1,"projects":[
                 {"name":"alpha","path":"/one/db.sqlite","project_key":"k","project_uuid":"u1"},
                 {"name":"alpha","path":"/two/db.sqlite","project_key":"k","project_uuid":"u2"}]}"#,
        )
        .unwrap();
        let r = load_from(&p).unwrap();
        let dups = r.duplicates();
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].1.len(), 2);
        assert_eq!(r.dangling().len(), 2, "neither path exists");
    }
}
