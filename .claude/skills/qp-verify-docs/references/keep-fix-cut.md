# Keep, fix, or cut — and where the fix lands

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

**The cut test does not apply to a module's `//!` summary line.** A one-line
`//!` header is the module's row in the rustdoc module list; a module with no
prose worth expanding is *correct* at one line, not under-documented, and
`tests/docs.rs` enforces exactly that shape. Never delete a summary line to
satisfy the cut test — improve the wording or leave it. Deleting a pointer
target (`docs/modules/*.md`) is worse: `include_str!` resolves at compile
time, so the build breaks. Cut applies to prose *inside* a header or markdown
file, never to the last line standing.

Deletion is also the right move for a stale sentence you cannot repair
precisely. **Prefer deleting over rewriting into vagueness.** Softening
`refresh_ready is the only function that computes readiness` into "readiness is
computed in a few places" produces a claim that is unfalsifiable, survives every
future sweep, and tells a reader nothing — it does not fix the drift, it hides
it permanently. You have two honest moves: go find every call site and write
the exact claim, or cut the sentence and file a ticket. Vagueness is not the
compromise between them; it is worse than either. Before you write a hedge
(`generally`, `a few`, `mostly`, `usually`, `in some cases`), treat it as a
signal you stopped reading the code too early and go back.

Deleting is a finding. Report what you removed and why, the same as a fix. Cut
plus ticket is the one case where you do both: the deletion is mechanical (the
claim is false either way), the replacement wording is the judgement call.

## What to do with a finding

This boundary is the same in a targeted run and in a sweep. Draw it per claim,
not per file.

**Fix in place** when the correct statement is recoverable from the code or
tests with no judgement — a renamed function, a moved module, a changed exit
code, a stale row in a table, a missing entry in a list the code enumerates.
Test: could you have derived the replacement wording by reading, without
choosing? If yes, fix it.

**File a ticket** when any of these hold:

- the fix requires choosing between defensible alternatives (two wordings, two
  places the sentence could live),
- the doc and the code disagree about *intent*, not detail,
- the doc is right and the *code* looks wrong,
- the fix is structural — redrawing a diagram, resequencing a section, merging
  two headers — even when every individual fact in it is obvious.

```
qp add "<what drifted>" --tag kind:docs --tag docs-audit --description "..."
```

Include the file, the section, the claim as written, and what the code
actually does. No line numbers.

If you catch yourself weighing which of two rewordings reads better, you are
past the boundary: stop and file the ticket.

**Say nothing** when the claim is correct. Do not manufacture findings to look
thorough — a clean result is a real result.

## Where the prose actually lives

When you fix in place, edit the right file. Code and documentation are
deliberately separate for the long headers. Two shapes exist:

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
  summary and the first line of the markdown concatenate, and the module-list
  summary becomes a run-on.
- **A pointer file is a build dependency.** `include_str!` resolves at compile
  time, so renaming or deleting it breaks the build.

Never move prose out to markdown just to shorten a `.rs` — for a one-line
header the pointer costs more than the content it replaces.

## Rules for anything you write

- **Never reference line numbers.** They rot within a week. Files and function
  names only. This rule is broken often — check your own output for it.
- Fenced code blocks in doc comments are parsed as Rust and doctested. Tag them
  ` ```text ` or ` ```rust,ignore ` or they will produce build warnings.
- Angle-bracket placeholders (`<duration>`) in doc comments are swallowed as
  unknown HTML tags. Put them inside backticks or a fenced block.
