//! `qp project` — the name a store answers to, and the machine-local registry of it.
//!
#![doc = include_str!("../../docs/modules/project_cmd.md")]

use crate::db;
use crate::outcome::{emit, Outcome};
use crate::registry::{self, Entry, Registered};
use anyhow::Result;
use clap::{Args, Subcommand};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub op: ProjectOp,
}

#[derive(Subcommand, Debug)]
pub enum ProjectOp {
    /// Put this store in the machine-local registry under its stored name
    Register {
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// List registered projects, flagging entries whose store has gone
    List {
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Resolve a project name to the store file it names
    Lookup {
        /// The name to resolve.
        name: String,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Drop a registry entry (the remedy for a stale or dangling one)
    Unregister {
        /// The name to drop.
        name: String,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
}

impl ProjectArgs {
    /// Whether `--json` was passed on whichever subcommand was chosen.
    pub fn json(&self) -> bool {
        match &self.op {
            ProjectOp::Register { json }
            | ProjectOp::List { json }
            | ProjectOp::Lookup { json, .. }
            | ProjectOp::Unregister { json, .. } => *json,
        }
    }
}

#[derive(Serialize)]
struct RegisterOutcome {
    name: String,
    path: String,
    project_key: String,
    registry: String,
    result: Registered,
}

impl Outcome for RegisterOutcome {
    fn human(&self) -> String {
        let what = match self.result {
            Registered::Added => "registered",
            Registered::Unchanged => "already registered",
            Registered::Updated => "registry entry updated",
        };
        format!("{what}: {} → {}", self.name, self.path)
    }
}

#[derive(Serialize)]
struct ListedEntry {
    name: String,
    path: String,
    project_key: String,
    /// True when the store file named by this entry is no longer there.
    dangling: bool,
}

#[derive(Serialize)]
struct ListOutcome {
    registry: String,
    projects: Vec<ListedEntry>,
}

impl Outcome for ListOutcome {
    fn human(&self) -> String {
        if self.projects.is_empty() {
            return format!(
                "no projects registered in {} — run `qp project register` in a store",
                self.registry
            );
        }
        let width = self
            .projects
            .iter()
            .map(|p| p.name.len())
            .max()
            .unwrap_or(0);
        let mut out = vec![self.registry.clone()];
        for p in &self.projects {
            out.push(format!(
                "  {:<width$}  {}{}",
                p.name,
                p.path,
                if p.dangling { "  DANGLING" } else { "" },
                width = width
            ));
        }
        out.join("\n")
    }
}

#[derive(Serialize)]
struct LookupOutcome {
    name: String,
    path: String,
    project_key: String,
    project_uuid: String,
}

impl Outcome for LookupOutcome {
    fn human(&self) -> String {
        self.path.clone()
    }
}

#[derive(Serialize)]
struct UnregisterOutcome {
    name: String,
    path: String,
    registry: String,
}

impl Outcome for UnregisterOutcome {
    fn human(&self) -> String {
        format!("unregistered {} ({})", self.name, self.path)
    }
}

pub fn run(db_path: &Path, a: ProjectArgs) -> Result<()> {
    match a.op {
        ProjectOp::Register { json } => register(db_path, json),
        ProjectOp::List { json } => list(json),
        ProjectOp::Lookup { name, json } => lookup(&name, json),
        ProjectOp::Unregister { name, json } => unregister(&name, json),
    }
}

fn register(db_path: &Path, json: bool) -> Result<()> {
    let conn = db::open(db_path)?;
    let name = crate::project::require_name(&conn)?;
    let entry = Entry {
        name: name.clone(),
        path: absolute(db_path)?,
        project_key: crate::project::require(&conn)?,
        project_uuid: crate::project::store_uuid(&conn)?,
    };
    let mut reg = registry::load()?;
    let result = reg.register(entry.clone())?;
    if result != Registered::Unchanged {
        reg.save()?;
    }
    emit(
        json,
        &RegisterOutcome {
            name,
            path: entry.path.display().to_string(),
            project_key: entry.project_key,
            registry: reg.path.display().to_string(),
            result,
        },
    )
}

fn list(json: bool) -> Result<()> {
    let reg = registry::load()?;
    let projects = reg
        .entries
        .iter()
        .map(|e| ListedEntry {
            name: e.name.clone(),
            path: e.path.display().to_string(),
            project_key: e.project_key.clone(),
            dangling: !e.path.exists(),
        })
        .collect();
    emit(
        json,
        &ListOutcome {
            registry: reg.path.display().to_string(),
            projects,
        },
    )
}

/// Resolve a name, or fail loudly — the property the whole slice exists for.
///
/// A name that is not registered is `not_found`, never a fallback to the store
/// under the cwd: silently answering with SOME store is how a caller ends up
/// reading a real ticket from the wrong project, which looks like success.
fn lookup(name: &str, json: bool) -> Result<()> {
    let reg = registry::load()?;
    let e = reg.lookup(name).cloned().ok_or_else(|| {
        db::not_found(
            format!(
                "no project named `{name}` in {}. `qp project list` shows what is registered; \
                 `qp project register` adds a store.",
                reg.path.display()
            ),
            None,
        )
    })?;
    if !e.path.exists() {
        return Err(db::not_found(
            format!(
                "project `{name}` is registered at {}, and there is no store there. \
                 Re-register it from its new location, or `qp project unregister {name}`.",
                e.path.display()
            ),
            None,
        ));
    }
    emit(
        json,
        &LookupOutcome {
            name: e.name,
            path: e.path.display().to_string(),
            project_key: e.project_key,
            project_uuid: e.project_uuid,
        },
    )
}

fn unregister(name: &str, json: bool) -> Result<()> {
    let mut reg = registry::load()?;
    let e = reg.unregister(name)?;
    reg.save()?;
    emit(
        json,
        &UnregisterOutcome {
            name: e.name,
            path: e.path.display().to_string(),
            registry: reg.path.display().to_string(),
        },
    )
}

/// An absolute path for the store, whether or not every component resolves.
///
/// `canonicalize` where it works, so two spellings of one store (a symlinked
/// checkout, a relative `--db`) register as one entry rather than two. It needs
/// the file to exist, which it does by the time this runs — `db::open` created
/// it — but a failure here must not cost the registration, so the fallback is
/// the plain absolute form.
fn absolute(p: &Path) -> Result<PathBuf> {
    if let Ok(c) = p.canonicalize() {
        return Ok(c);
    }
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(p))
}
