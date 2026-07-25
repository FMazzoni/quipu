//! Install the bundled skills into Claude Code's skill directory.
//!
#![doc = include_str!("../../docs/modules/install_skills.md")]

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;

/// Every file under `skills/`, baked into the binary.
///
/// Keys are paths relative to `skills/`; `embedded_set_matches_skills_tree`
/// asserts this list is the whole tree, because a missing entry ships a
/// silently incomplete skill.
const EMBEDDED_SKILLS: &[(&str, &str)] = &[
    ("wave/SKILL.md", include_str!("../../skills/wave/SKILL.md")),
    (
        "report-render/SKILL.md",
        include_str!("../../skills/report-render/SKILL.md"),
    ),
];

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

    match source_root()? {
        Some(src_root) => install_from_disk(&src_root, &target, a.copy),
        None => install_embedded(&target),
    }
}

/// The on-disk `skills/` directory, if one is usable.
///
/// `QP_SKILLS_SRC` wins when set and is a hard error when it points nowhere —
/// an explicit override that silently falls back to the snapshot is worse than
/// a failure. Otherwise the `current_exe()` derivation is *probed*: it resolves
/// to `~/skills` for an installed binary, so a bare `exists()` check is the
/// whole difference between the dev case and the distributed case.
fn source_root() -> Result<Option<PathBuf>> {
    if let Some(v) = std::env::var_os("QP_SKILLS_SRC") {
        let root = PathBuf::from(v).join("skills");
        if !root.is_dir() {
            anyhow::bail!(
                "QP_SKILLS_SRC is set but {} is not a directory",
                root.display()
            );
        }
        return Ok(Some(root));
    }
    let derived = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("../../skills")))
        .filter(|p| p.is_dir());
    Ok(derived)
}

/// Live mode — link (or copy) each `skills/<name>/` out of a real checkout.
fn install_from_disk(
    src_root: &std::path::Path,
    target: &std::path::Path,
    copy: bool,
) -> Result<()> {
    for entry in
        std::fs::read_dir(src_root).with_context(|| format!("reading {}", src_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let dst = target.join(format!("qp-{}", name.to_string_lossy()));
        replace_target(&dst)?;
        let mode = if copy {
            copy_dir_recursive(&entry.path(), &dst)?;
            "copied"
        } else {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(entry.path(), &dst)?;
                "linked"
            }
            #[cfg(not(unix))]
            {
                copy_dir_recursive(&entry.path(), &dst)?;
                "copied"
            }
        };
        println!(
            "installed ({mode}) {} -> {}",
            entry.path().display(),
            dst.display()
        );
    }
    Ok(())
}

/// Snapshot mode — write `EMBEDDED_SKILLS` out, for a binary with no checkout.
fn install_embedded(target: &std::path::Path) -> Result<()> {
    let mut installed: Vec<String> = Vec::new();
    for (rel, body) in EMBEDDED_SKILLS {
        let (skill, rest) = rel
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("embedded skill path {rel:?} has no directory"))?;
        let dst_root = target.join(format!("qp-{skill}"));
        if !installed.iter().any(|s| s == skill) {
            replace_target(&dst_root)?;
            installed.push(skill.to_string());
        }
        let dst = dst_root.join(rest);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dst, body).with_context(|| format!("writing {}", dst.display()))?;
    }
    for skill in &installed {
        println!(
            "installed (embedded snapshot) {skill} -> {}",
            target.join(format!("qp-{skill}")).display()
        );
    }
    Ok(())
}

/// Clears a destination so the install is a replace, not a merge.
fn replace_target(dst: &std::path::Path) -> Result<()> {
    guard_destructive_target(dst)?;
    let _ = std::fs::remove_file(dst);
    let _ = std::fs::remove_dir_all(dst);
    Ok(())
}

/// Defence-in-depth before a recursive remove.
///
/// Refuses a path that is too shallow (e.g. a relative path resolved against
/// an unexpected cwd) or that doesn't look like one of our own `qp-<name>`
/// install targets.
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

#[cfg(test)]
mod tests {
    use super::EMBEDDED_SKILLS;
    use std::path::{Path, PathBuf};

    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read_dir skills/") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                out.push(
                    path.strip_prefix(base)
                        .expect("under skills/")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }

    /// The embedded set is the whole `skills/` tree.
    ///
    /// `include_str!` is hand-listed, so a new file — `wave/references/x.md`,
    /// say — is embedded by nobody and ships missing from every released
    /// binary with nothing failing. This is the only thing that notices.
    #[test]
    fn embedded_set_matches_skills_tree() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills");
        let mut on_disk = Vec::new();
        walk(&root, &root, &mut on_disk);
        on_disk.sort();
        assert!(!on_disk.is_empty(), "found no files under skills/");

        let mut embedded: Vec<String> =
            EMBEDDED_SKILLS.iter().map(|(p, _)| p.to_string()).collect();
        embedded.sort();

        assert_eq!(
            embedded, on_disk,
            "EMBEDDED_SKILLS and the skills/ tree disagree. Add an include_str! \
             entry for every new file (or drop the stale one) — a file missing \
             here is absent from the standalone binary and nothing else fails."
        );
    }

    /// Embedded bodies are the files themselves, not a stale paste.
    #[test]
    fn embedded_bodies_match_disk() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills");
        for (rel, body) in EMBEDDED_SKILLS {
            let disk = std::fs::read_to_string(root.join(rel))
                .unwrap_or_else(|e| panic!("read skills/{rel}: {e}"));
            assert_eq!(&disk, body, "skills/{rel} differs from the embedded copy");
        }
    }
}
