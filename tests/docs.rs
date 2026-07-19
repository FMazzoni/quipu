//! Mechanically enforces the `//!` module-header conventions across `src/**/*.rs`.
//!
//! Why a test and not a lint or a shell script: it runs under `cargo test`, so
//! CI picks it up with no new wiring, no new deps, and nothing external.
//!
//! The load-bearing rule is rule 4, the blank `//!` before every include_str
//! pointer. Without it rustdoc merges the one-line summary with the first
//! paragraph of the included markdown, and the module-list entry becomes a
//! run-on that still renders as valid HTML — so nothing but a reader's eye
//! catches it. It has regressed twice (d5da239, and again during QP-133).
//!
//! This test reads SOURCE, never rendered HTML. Asserting against rustdoc
//! output is tempting and wrong: rustdoc injects `<wbr>` into long module
//! names (`install_<wbr>skills`), so a naive name regex over the HTML produces
//! a false PASS on exactly the module most likely to drift.
//!
//! Asymmetry worth knowing: rule 3 is only checkable in one direction. Six
//! modules (`log`, `status`, `tag`, `timeline`, `tree`, `wave`) are correctly
//! one-line with no `.md` and no pointer. Nothing in the source distinguishes
//! "correctly single-line" from "detail was lost on the way here", so this
//! test catches prose that should have moved out and never catches prose that
//! should have existed.

use std::fs;
use std::path::{Path, PathBuf};

/// `src/main.rs` is exempt from rules 2 and 3 (one-line summary, no inline
/// detail) and from those two only.
///
/// The rationale behind rules 2 and 3 is the module list: rustdoc uses
/// everything before the first blank `//!` as the one-line summary in the
/// table of modules, so a multi-line first paragraph renders there as a wall
/// of text. The crate root has no such row — its doc *is* the front page. The
/// exit-code table is reference material a reader wants inline, on the page
/// they land on, not one hop away in `docs/architecture.md`.
///
/// Rules 1, 4, 5 and 6 still apply to it: it must still open with a summary,
/// must still keep the blank `//!` before its pointer, and its pointer target
/// must still exist.
const CRATE_ROOT_EXEMPT: &str = "src/main.rs";

/// Max length of the summary line, excluding the `//! ` prefix.
const MAX_SUMMARY: usize = 100;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The contiguous run of `//!` / `#![doc = ...]` lines at the top of a file.
///
/// Stops at the first line that is neither, so a file with no header yields an
/// empty block and rule 1 reports it.
fn header_lines(src: &str) -> Vec<&str> {
    src.lines()
        .take_while(|l| {
            let t = l.trim_start();
            t.starts_with("//!") || t.starts_with("#![doc")
        })
        .collect()
}

fn is_blank_doc(line: &str) -> bool {
    line.trim() == "//!"
}

fn is_pointer(line: &str) -> bool {
    line.trim_start().starts_with("#![doc")
}

/// Extracts the path argument of an `include_str!("...")` on a pointer line.
fn pointer_target(line: &str) -> Option<&str> {
    let (_, rest) = line.split_once("include_str!(\"")?;
    let (target, _) = rest.split_once('"')?;
    Some(target)
}

/// One assertion per rule, collected so a single run reports every violation
/// in the tree rather than only the first.
#[test]
fn module_headers_follow_convention() {
    let root = repo_root();
    let mut files = Vec::new();
    rust_sources(&root.join("src"), &mut files);
    assert!(!files.is_empty(), "found no .rs files under src/");

    let mut failures: Vec<String> = Vec::new();

    for path in &files {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let src = fs::read_to_string(path).expect("read source file");
        let header = header_lines(&src);
        let mut fail = |msg: String| failures.push(format!("{rel}: {msg}"));

        // Rule 1 — every file opens with a `//!` summary.
        let Some(first) = header.first() else {
            fail(
                "no `//!` module header. Add one as the very first line: a single \
                 sentence saying what this module is. For a command module that is \
                 the state-machine edge it implements (e.g. the `assigned` → \
                 `running` edge)."
                    .to_string(),
            );
            continue;
        };
        if !first.trim_start().starts_with("//!") || is_blank_doc(first) {
            fail(format!(
                "first header line is not a `//!` summary (found `{}`). The summary \
                 must be the very first line of the file.",
                first.trim()
            ));
            continue;
        }

        // Rule 5 — the summary stays short enough to read in a module table.
        let summary = first.trim_start().trim_start_matches("//!").trim();
        if summary.len() > MAX_SUMMARY {
            fail(format!(
                "summary is {} chars, over the {MAX_SUMMARY}-char budget. Shorten it \
                 and move the detail to docs/modules/{stem}.md. Summary was: {summary:?}",
                summary.len()
            ));
        }

        // Rule 4 — a blank `//!` immediately precedes every pointer.
        //
        // Checked for every file including the crate root: this is the rule
        // that has actually regressed, and no file is exempt from it.
        for (i, line) in header.iter().enumerate() {
            if !is_pointer(line) {
                continue;
            }
            let preceded_by_blank = i > 0 && is_blank_doc(header[i - 1]);
            if !preceded_by_blank {
                fail(format!(
                    "the `#![doc = include_str!(...)]` pointer on header line {} is not \
                     preceded by a blank `//!` line. Insert one. Without it rustdoc \
                     glues the summary onto the first paragraph of the included \
                     markdown and the module-list entry becomes a run-on — which still \
                     renders as valid HTML, so only a reader catches it.",
                    i + 1
                ));
            }
        }

        // Rule 6 — every pointer target exists on disk.
        for line in &header {
            let Some(target) = pointer_target(line) else {
                continue;
            };
            let resolved = path.parent().unwrap().join(target);
            if !resolved.exists() {
                fail(format!(
                    "include_str! target {target:?} does not exist (resolved to {}). \
                     Create it or fix the path.",
                    resolved.display()
                ));
            }
        }

        if rel == CRATE_ROOT_EXEMPT {
            // Exempt from rules 2 and 3 only — see CRATE_ROOT_EXEMPT.
            continue;
        }

        // Rule 2 — the summary is ONE line: followed by a blank `//!`, or the
        // header ends right there.
        if let Some(second) = header.get(1) {
            if !is_blank_doc(second) {
                fail(format!(
                    "the summary runs onto a second line (`{}`). rustdoc uses \
                     everything before the first blank `//!` as the module-list \
                     summary, so this renders as a wall of text in the table. Put a \
                     blank `//!` after the first line and move the rest to \
                     docs/modules/{stem}.md.",
                    second.trim()
                ));
            }
        }

        // Rule 3 — no inline detail. After the summary the only permitted
        // lines are blank `//!` and the pointer.
        for (i, line) in header.iter().enumerate().skip(1) {
            if is_blank_doc(line) || is_pointer(line) {
                continue;
            }
            fail(format!(
                "header line {} is inline prose (`{}`). After the summary the only \
                 permitted lines are a blank `//!` and a \
                 `#![doc = include_str!(\"...\")]` pointer. Move this prose to \
                 docs/modules/{stem}.md and point at it.",
                i + 1,
                line.trim()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "module header convention violations ({}):\n\n{}\n",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Guards the guard: if the exempt path stops naming a real file, the
/// exemption has silently widened or gone stale.
#[test]
fn crate_root_exemption_names_a_real_file() {
    let path = repo_root().join(CRATE_ROOT_EXEMPT);
    assert!(
        path.exists(),
        "CRATE_ROOT_EXEMPT points at {CRATE_ROOT_EXEMPT}, which does not exist. \
         Update or delete the exemption in tests/docs.rs."
    );
}
