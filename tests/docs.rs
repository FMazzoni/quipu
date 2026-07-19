//! Mechanically enforces the `//!` module-header and `///` item-doc conventions
//! across `src/**/*.rs`.
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
//! No file is exempt, including the crate root. `src/main.rs` used to be, on
//! the grounds that its exit-code table was "reference material a reader wants
//! inline, on the page they land on, not one hop away in
//! `docs/architecture.md`". That premise was false: `include_str!` splices
//! `architecture.md` into the crate page itself, so the inline copy and
//! `architecture.md`'s own `## Exit codes` section rendered twenty lines apart
//! on `qp/index.html` and drifted apart in content (QP-169). Reintroducing an
//! exemption needs a rationale that survives looking at the rendered page.
//!
//! Asymmetry worth knowing: rule 3 is only checkable in one direction. Three
//! modules (`status`, `tag`, `tree`) are correctly one-line with no `.md` and
//! no pointer. Nothing in the source distinguishes
//! "correctly single-line" from "detail was lost on the way here", so this
//! test catches prose that should have moved out and never catches prose that
//! should have existed.
//!
//! Rule 7 extends the same budget to `///` item docs, for the same reason: the
//! item table on a module page prints the entire first paragraph untruncated,
//! so a multi-line first paragraph on `pub fn open` is the identical wall of
//! text rules 2 and 4 exist to prevent, one page down. Rules 1–6 read only the
//! contiguous header block at the top of a file; rule 7 is the only one that
//! scans the whole body.
//!
//! Rule 7 governs `pub` *items* — `fn`, `struct`, `enum`, `trait`, `const` and
//! the rest of `DOC_ITEM_KEYWORDS` — and not `pub` struct fields, which parse
//! the same way and are excluded deliberately. A field renders on its struct's
//! page as a definition list entry carrying its doc in full; it never appears
//! in a summary table, so there is nothing for the budget to protect. Three
//! fields exceed 100 chars today (`cmd/decisions.rs`, `cmd/report.rs`,
//! `cmd/wait.rs`) and read correctly where they render.
//!
//! Known gap: rule 7 checks `pub` items only, and quipu is a binary crate, so
//! rustdoc documents private items too — they get rows in the same table. Nine
//! private items exceed the budget today, `db::migrate` (370 chars) worst.
//! Dropping the `pub` requirement is the fix and is a one-line change to
//! `is_documented_pub_item`; it was left out of QP-160 because the offenders
//! sit in files that slice did not own.
//!
//! A rule requiring *every* `pub` item to carry a doc was considered and
//! rejected. It fires on 29 items, ~20 of which are the uniform command
//! entrypoint `pub fn run(db_path, args)` — one per `src/cmd/*.rs`. Satisfying
//! it means twenty lines of "Run the add command.", which is the restating-the-
//! signature anti-pattern the module-header guidance already bans, minted at
//! scale and mechanically enforced. Rule 7 caps prose that exists; it does not
//! demand prose that does not.

use std::fs;
use std::path::{Path, PathBuf};

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

/// Item keywords rule 7 applies to.
///
/// A `pub` line whose next token is not one of these is a struct field
/// (`pub since_id: Option<i64>,`). Fields render on the struct page as a
/// definition list with the doc in full — there is no summary table to blow
/// out, so the budget that motivates rule 7 does not apply to them.
const DOC_ITEM_KEYWORDS: &[&str] = &[
    "fn", "struct", "enum", "trait", "const", "static", "type", "mod", "union", "macro", "use",
];

/// Whether a source line declares a `pub` item rule 7 governs.
fn is_documented_pub_item(line: &str) -> bool {
    let t = line.trim_start();
    let rest = if let Some(r) = t.strip_prefix("pub(") {
        match r.split_once(')') {
            Some((_, after)) => after,
            None => return false,
        }
    } else {
        match t.strip_prefix("pub") {
            Some(r) => r,
            None => return false,
        }
    };
    let Some(word) = rest.split_whitespace().next() else {
        return false;
    };
    DOC_ITEM_KEYWORDS.contains(&word)
}

/// The first paragraph of a `///` block, joined into the single line rustdoc
/// renders it as.
///
/// Everything up to the first blank `///`; that is the span rustdoc lifts into
/// the item table, so it is the span rule 7 measures.
fn first_doc_paragraph(block: &[&str]) -> String {
    block
        .iter()
        .map(|l| l.trim_start().trim_start_matches("///").trim())
        .take_while(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Rule 7 — the first paragraph of a `///` block on a `pub` item fits the
/// module-table budget.
///
/// Separate from the header test because it is the only rule that scans past
/// the header block: `///` docs are anywhere in the file.
#[test]
fn item_docs_follow_convention() {
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
        let src = fs::read_to_string(path).expect("read source file");
        let lines: Vec<&str> = src.lines().collect();

        let mut i = 0;
        while i < lines.len() {
            if !lines[i].trim_start().starts_with("///") {
                i += 1;
                continue;
            }
            let start = i;
            while i < lines.len() && lines[i].trim_start().starts_with("///") {
                i += 1;
            }
            let block = &lines[start..i];

            // Attributes sit between the doc block and the item it documents.
            let mut j = i;
            while j < lines.len() && lines[j].trim_start().starts_with('#') {
                j += 1;
            }
            let Some(item) = lines.get(j) else { continue };
            if !is_documented_pub_item(item) {
                continue;
            }
            let item = item.trim_start();

            let para = first_doc_paragraph(block);
            if para.len() > MAX_SUMMARY {
                failures.push(format!(
                    "{rel}:{}: the first paragraph of the `///` block on `{}` is {} \
                     chars, over the {MAX_SUMMARY}-char budget. The item table on the \
                     module page prints the whole first paragraph untruncated, so this \
                     renders there as a wall of text — the same failure rules 2 and 4 \
                     prevent for module headers. Split it: a one-line summary naming \
                     the subject, a blank `///`, then the existing detail. Keep the \
                     detail. Paragraph was: {para:?}",
                    start + 1,
                    item.trim_end_matches('{').trim(),
                    para.len(),
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "item doc convention violations ({}):\n\n{}\n",
        failures.len(),
        failures.join("\n\n")
    );
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
        // This is the rule that has actually regressed, twice.
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
