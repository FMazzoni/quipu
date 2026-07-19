//! Install the bundled skills into Claude Code's skill directory.
//!
#![doc = include_str!("../../docs/modules/install_skills.md")]

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct InstallSkillsArgs {
    /// Target directory (defaults to ~/.claude/skills)
    #[arg(long)]
    pub target: Option<PathBuf>,
    /// Copy instead of symlink
    #[arg(long)]
    pub copy: bool,
}

pub fn run(a: InstallSkillsArgs) -> Result<()> {
    let src_root = {
        let base = std::env::var_os("QP_SKILLS_SRC")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("../../")))
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "could not determine skills source root: current_exe() failed and QP_SKILLS_SRC is not set"
                )
            })?;
        // QP_SKILLS_SRC (or the derived base) is the project root; skills live in base/skills/
        base.join("skills")
    };
    let target = match a.target {
        Some(t) => t,
        None => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| !p.as_os_str().is_empty())
                .ok_or_else(|| anyhow::anyhow!("HOME is not set; pass --target explicitly"))?;
            home.join(".claude/skills")
        }
    };
    std::fs::create_dir_all(&target)?;
    for entry in
        std::fs::read_dir(&src_root).with_context(|| format!("reading {}", src_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let dst = target.join(format!("qp-{}", name.to_string_lossy()));
        guard_destructive_target(&dst)?;
        let _ = std::fs::remove_file(&dst);
        let _ = std::fs::remove_dir_all(&dst);
        if a.copy {
            copy_dir_recursive(&entry.path(), &dst)?;
        } else {
            #[cfg(unix)]
            std::os::unix::fs::symlink(entry.path(), &dst)?;
            #[cfg(not(unix))]
            copy_dir_recursive(&entry.path(), &dst)?;
        }
        println!("installed {} -> {}", entry.path().display(), dst.display());
    }
    Ok(())
}

/// Defence-in-depth: refuse to remove a path that is too shallow (e.g. a
/// relative path resolved against an unexpected cwd) or that doesn't look
/// like one of our own `qp-<name>` install targets.
fn guard_destructive_target(dst: &std::path::Path) -> Result<()> {
    let name_ok = dst
        .file_name()
        .map(|n| n.to_string_lossy().starts_with("qp-"))
        .unwrap_or(false);
    if dst.components().count() < 3 || !name_ok {
        anyhow::bail!(
            "refusing to remove suspicious path {}: too shallow or missing qp- prefix",
            dst.display()
        );
    }
    Ok(())
}

fn copy_dir_recursive(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dst = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dst)?;
        } else {
            std::fs::copy(entry.path(), dst)?;
        }
    }
    Ok(())
}
