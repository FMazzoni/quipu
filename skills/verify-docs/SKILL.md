---
name: verify-docs
description: Verify documentation against the code it describes, then fix what drifted or file it as a ticket. Use when a doc statement looks wrong while reading, or to sweep all docs for staleness. Invoked as `/qp-verify-docs <file>[#<anchor>]` or `/qp-verify-docs --sweep`.
---

## Why this exists

quipu's prose documentation is written by an LLM about code the owner cannot
fully audit. That is a real risk: a confident, wrong doc is worse than no doc.
This skill is how a claim gets challenged.

Two modes, and they are not interchangeable:

- **Targeted** — a human noticed something wrong while reading and copied a
  command. A *reporting shortcut*.
- **Sweep** — systematic drift detection across all documented files. The
  actual *detector*.

A targeted run proves nothing about the rest of the docs. Do not report "docs
verified" after one.

## Invocation

```
/qp-verify-docs docs/architecture.md#three-concepts   # one section
/qp-verify-docs src/cmd/claim.rs                      # one file's //! header
/qp-verify-docs --sweep                               # every documented file
```

The argument is deliberately minimal — the caller may have typed extra context
after pasting it. Read anything they added; it is usually the specific
complaint and the most valuable input you have.

### Resolving `#<anchor>`

Anchors are rustdoc heading slugs: lowercased heading text, non-alphanumerics
collapsed to `-`. `#three-concepts` is `## Three concepts`. Find the matching
heading in the file and verify from there to the next heading of the same or
higher level. If the anchor matches nothing, say so and verify the whole file
rather than guessing.

## Where the prose actually lives

Code and documentation are deliberately separate for the long headers. Two
shapes exist, and you must edit the right file:

**Inline** — most modules. The whole `//!` header sits in the `.rs`. Fix it
there.

**Pointer** — modules with substantial prose. A one-line `//!` summary stays in
the `.rs` so the module list and a reader of the source both still see what it
is, and the detail lives in markdown:

```rust,ignore
//! Canonical queries over the qp schema.
//!
#![doc = include_str!("../docs/modules/store.md")]
```

For these, **edit the markdown, not the `.rs`**. The `.rs` holds only the
summary line. `docs/architecture.md` is the same pattern at crate level, pulled
in from `src/main.rs`.

Two things that break silently here:

- **The blank `//!` line before the pointer is load-bearing.** Without it the
  summary and the first line of the markdown concatenate into one paragraph,
  and the module-list summary becomes a run-on.
- **A pointer file is a build dependency.** Renaming or deleting it breaks the
  build — which is the point, but means you cannot simply drop a stale doc.

Never move prose out to markdown just to shorten a `.rs`. The threshold is
whether there is real prose: for a one-line header the pointer costs more than
the content it replaces, and adds a file hop for no gain.

## What to check

Work claim by claim. For each statement, decide which kind it is:

**Behavioural claims** — what the system does. `qp wait --cohort-done exits 4
on an empty cohort`. These are verifiable and must be traced to one of:
- a test in `tests/` that asserts it, or
- a command you can actually run.

If a behavioural claim has neither, that is itself a finding: either the claim
is wrong, or a test is missing.

**Structural claims** — how the code is arranged. `refresh_ready is the only
function that computes readiness`. Verify by reading the code. These rot
fastest, because refactors move code without changing behaviour.

**Rationale** — why a decision was made. Not verifiable from code. Check it
against `$QUIPU_VAULT/decisions/` and qp decision events (`qp decisions`), not
against the source. Leave it alone if it merely sounds outdated.

## Rules for anything you write

- **Never reference line numbers.** They rot within a week. Files and function
  names only. This rule has been broken repeatedly — including in the tickets
  that created this skill — so check your own output for it.
- Fenced code blocks in doc comments are parsed as Rust and doctested. Tag them
  ` ```text ` or ` ```rust,ignore ` or they will produce build warnings.
- Angle-bracket placeholders (`<duration>`) in doc comments are swallowed as
  unknown HTML tags. Put them inside backticks or a fenced block.

## Keep, fix, or cut

Correctness is not the only question. A sentence can be accurate and still not
worth the maintenance. Before you fix a drifted claim, decide whether it earns
the repair.

**The test:** if this code were rewritten tomorrow in a different shape, would
the sentence still be true *and* still be useful? Prose that only describes the
current arrangement of lines will rot again after the next refactor. Prose that
records something the code cannot say for itself survives it.

**Keep** — it carries one of the four durable kinds:

- **WHY** — the reason the code has this shape, and what was rejected. A reader
  can see what `with_tx` does; they cannot see that read-then-write was banned
  deliberately.
- **INVARIANTS** — what must stay true, and what breaks when it does not.
  Prefer the ones that state a consequence; if a claim states only the rule,
  adding the consequence is a legitimate fix.
- **GOTCHAS** — the thing that looks wrong but is correct, or looks safe but is
  not. The blank `//!` line before a `#![doc = include_str!]` pointer is one:
  invisible, load-bearing, and a reader will delete it.
- **BOUNDARIES** — what deliberately does *not* belong in this module, and where
  it lives instead.

**Fix** — durable content, wrong details. A renamed function, a moved module, a
changed exit code inside a sentence that is otherwise worth having.

**Cut** — it restates the code. `/// Returns the task id` above
`fn task_id() -> i64` is negative value: staleness surface carrying no
information. If deleting a sentence would lose nothing a reader could not get
from the signature or the body, delete it rather than re-verifying it every
sweep.

Deletion is also the right move for a stale sentence you cannot repair
precisely. **Prefer deleting over rewriting into vagueness** — softening
`refresh_ready is the only function that computes readiness` into "readiness is
computed in a few places" produces a claim that is unfalsifiable, survives every
future sweep, and tells a reader nothing. Cut it and, if the real behaviour
matters, file a ticket.

Deleting is a finding. Report what you removed and why, the same as a fix.

## What to do with a finding

**Fix in place** when the correct statement is unambiguous from the code — a
renamed function, a moved module, a changed exit code.

**File a ticket** when the fix requires a judgement call, when the doc and the
code disagree about intent, or when the doc is right and the *code* looks
wrong:

```
qp add "<what drifted>" --tag kind:docs --tag docs-audit --description "..."
```

Include the file, the section, the claim as written, and what the code
actually does. No line numbers.

**Say nothing** when the claim is correct. Do not manufacture findings to look
thorough — a clean result is a real result.

## Sweep mode

Enumerate every file with a `//!` header plus every markdown file under
`docs/`. Check each one independently; a stale header in one file says nothing
about another.

Report a per-file verdict — `ok`, `drifted`, or `unverifiable` — and file one
ticket per drifted file. Then state plainly how many files you actually
checked, so a partial sweep is never mistaken for a complete one.

## Verifying afterwards

Whatever you changed, `cargo doc --no-deps` must produce no new warnings:

```
cargo doc --no-deps 2>&1 | grep -c "unclosed HTML"
```

Pre-existing warnings in `cmd/report.rs` are tracked separately; do not claim
them as yours or silently fix them under a docs ticket.
