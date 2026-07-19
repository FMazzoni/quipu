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

Read anything the caller typed after the argument; it is usually the specific
complaint and the most valuable input you have.

A targeted run names a source file, but the page is still where you look for
anything beyond the single claim: `just docs`, then
`w3m -dump -cols 100 target/doc/qp/<module>/index.html`. See "Source or
rendered page".

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
  summary and the first line of the markdown concatenate, and the module-list
  summary becomes a run-on.
- **A pointer file is a build dependency.** `include_str!` resolves at compile
  time, so renaming or deleting it breaks the build.

Never move prose out to markdown just to shorten a `.rs` — for a one-line
header the pointer costs more than the content it replaces.

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

**When `$QUIPU_VAULT` is unset or the path does not exist** — the vault is
external and per-machine, so this is the common case, not an error. Check
rationale against `qp decisions` alone, mark every claim you could not reach
`unverifiable`, and name the vault as the reason. Never edit or delete a
rationale claim you could not check — an unreachable source is not evidence the
claim is wrong. Keep verifying behavioural and structural claims as normal.

## Rules for anything you write

- **Never reference line numbers.** They rot within a week. Files and function
  names only. This rule is broken often — check your own output for it.
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

## House style

The sections above decide *whether* a sentence stays. This one decides how it
reads. `docs/architecture.md` is the exemplar — when this section and that file
disagree, the file wins.

**Write verifiable statements, not insight.** Every sentence should be something
a reader could check against the code and come back with true or false. This is
the load-bearing rule and the rest follow from it. Prose written to sound
authoritative is how `db.md` came to say "Four variants" above a list of five:
the sentence was shaped for cadence, and nobody counted. A sentence that cannot
be checked cannot be caught when it goes wrong.

**Write a reference, not an essay.** A reader arrives with a question and needs
an answer. Lead with the subject, state the mechanism plainly, and where a claim
is behavioural, name the test or code path that settles it — architecture.md
does this constantly (`read_command_self_heals_stale_schema_without_init`,
`resolve_path_finds_store_from_worktree`), and a named test turns a claim into
something the next sweep can verify in one command.

**Stay above the signature.** The cut test already bans restating the code. What
earns space is architecture and rationale a code reader cannot recover from the
code: why this shape, what was rejected, what breaks if it changes.

**Headings are noun phrases naming the subject.** They render into the rustdoc
sidebar, where a full sentence is unreadable as navigation. Not "What is here
because it must have exactly one implementation" but "Single-implementation
rules"; not "The error taxonomy is an agent-facing contract" but "Error
taxonomy". If a heading contains a verb phrase or reads as a claim, it is a
topic sentence that escaped into the heading — move it into the first line of
the section.

**Modules are shorter than architecture.md.** That file covers the whole system;
a module doc covers one file. Most should land well under 80 lines. Length here
is usually essay creep, not thoroughness.

**No dangling opener.** A markdown pointer file renders as the first sentence of
the page body, not as a continuation of the `//!` summary above it. An opening
"Also holds the error types…" reads as a fragment in rustdoc. Give it a subject:
"This module also holds the error types…".

**The hedge tripwire still applies.** `generally`, `mostly`, `usually`, `a few`,
`in some cases` — see "Keep, fix, or cut". A hedge added to make a sentence
survive a sweep is worse than deleting it.

### Before / after, from the `db.md` rewrite

```text
heading   - ## What is here because it must have exactly one implementation
          + ## Single-implementation rules

heading   - ## Two behaviours that look wrong and are not     [covered three
          + ## Migration on open                               subjects; split,
          + ## Path resolution                                 and path
                                                               resolution stopped
                                                               hiding under it]

opener    - Also holds the error types and the shared mutation utilities.
          + This module also holds the error types and the shared mutation
            utilities.

claim     - Four variants, not one per failure site … [then lists five]
          + `QuipuError` has five variants, split by *what a caller should do
            next* … Five variants, six kinds.
```

The last one is the point of the whole section: the original read well and was
false. Count the list.

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

## Source or rendered page: which one you read

Both get read, for different jobs. Confusing them produces either a false PASS
or a reverted skill, so the split is stated before anything else:

- **Mechanical tests read SOURCE, always.** `tests/docs.rs` checks `//!` header
  shape, summary length, the blank `//!` before every pointer, and `///` first
  paragraphs — by regex, over `src/**/*.rs`. Its own header explains why it must
  never read HTML: rustdoc injects `<wbr>` into long module names, so
  `install_skills` renders as `install_<wbr>skills` and a naive name regex over
  the HTML gives a false PASS on exactly the module most likely to drift. That
  warning is correct and stays. Do not "fix" `tests/docs.rs` to read rendered
  output.

- **Reading sweeps read the RENDERED PAGE.** A judgement about redundancy,
  ordering, or whether a copied command still runs is a judgement about the
  artifact a reader sees. No regex is involved, so the `<wbr>` hazard does not
  apply — an agent reading `install_skills` on the page reads it correctly.

These do not conflict. The rule is about method: pattern-matching needs source
because rendering perturbs the bytes; reading needs the page because
composition is where the defects live. Do not revert this skill to per-file
sweeping on the strength of the `tests/docs.rs` warning — it warns about
regexes, not about eyes.

**Why the page, concretely.** A module's rendered page is three sources merged:
its `//!` header, plus the `include_str!`'d `docs/modules/<name>.md`, plus an
item table built from the first paragraph of each `///`. The crate front page is
`main.rs`'s header plus all of `docs/architecture.md`. Per-file review sees one
third at a time, so every sentence can be true in isolation while the page is
redundant, misordered, or uncopyable. Two real misses prove it: a five-agent
per-file sweep passed a duplicated exit-code table (`main.rs`'s header restates
architecture.md's `## Exit codes` section, both on `qp/index.html`) and passed
rustdoc turning an unbackticked `--cohort-done` into `–cohort-done`, which does
not run when pasted.

## Sweep mode

Sweeps are per **rendered page**, not per source file. Build first:

```
just docs          # → target/doc/
just docs-dump     # every module page dumped as text, for skimming
```

**Dump pages; do not read raw HTML.** `w3m -dump -cols 100 <page>` is the tool.
It is installed and renders rustdoc cleanly at roughly half the bytes of the
HTML — the front page goes 44629 → 21131 — which is the difference between the
sweep fitting in context and not. `lynx`, `pandoc` and `html2text` are **not
installed**; do not reach for them.

**Establish the denominator first.** Enumerate the pages and count them:

```
find target/doc/qp -name 'index.html' | sort
```

That is the crate front page plus one page per module — 32 at this writing. Two
traps in the wider tree: `find target/doc -name '*.html'` picks up
`target/doc/src/`, the syntax-highlighted source mirror, which is not
documentation; and `find target/doc/qp -name '*.html'` stays out of that mirror
but still returns every per-item page (`struct.*.html`, `fn.*.html`), five times
the real count. Neither is the denominator. Restrict to `index.html`.

Never accept a page count from the prompt, a previous sweep, or a ticket — the
set grows every wave, and a briefed "23" against an actual 25 turns a 92% sweep
into a reported-complete one. If your enumeration disagrees with the number you
were given, say so and use yours.

Check each page independently; a stale claim on one says nothing about another.

### Page-level checks

These run *in addition to* the per-claim work in "What to check", which is
unchanged — every sentence on the page still gets classified and verified. What
follows is the set of defects that exist only at page scale:

- **Redundancy across the seam.** The `//!` header and the included markdown
  were written at different times by different agents. Does the header restate a
  section of the markdown? Said twice, a reader trusts neither copy once they
  drift. File this rather than fixing it: choosing which copy survives is a
  judgement call.

- **Commands that do not survive being copied.** Read every command on the page
  as if pasting it into a shell. rustdoc's markdown applies smart typography, so
  an unbackticked `--flag` becomes `–flag` and fails silently. Grep the dump for
  `–` adjacent to a word character; numeric ranges (`2–5`, `1–3 ms`) are correct
  typography, not findings. Check too that `<placeholder>` did not vanish as an
  unknown HTML tag — same class, different mechanism, both invisible in source.

- **Item-table walls of text.** The module page prints each `///` first
  paragraph untruncated, so one that runs several lines turns the table into
  prose. `tests/docs.rs` rule 7 catches the length; you catch whether the
  summary is a *summary*.

- **Order of answer.** A reader arrives with a question. Does the page answer in
  the order they would ask — what this is, then how it behaves, then why it has
  this shape — or does it open with a caveat about something they have not met
  yet? Resequencing is a ticket; see the structural clause under "File a
  ticket".

- **Composition gaps.** Does the header's one-line summary still describe what
  the markdown below it covers? The `.rs` summary and the `.md` body drift apart
  because nothing renders them together except this page.

When a finding is visible only on the page, say so in the report. That is the
evidence per-file sweeping would have missed it, and the reason this mode exists.

### Verdicts

A sweep does not change the fix-or-ticket boundary — apply it claim by claim as
you go. Fix the mechanical drift inline; file **one ticket per page that still
has unfixed drift** once you have. A page whose drift you fixed entirely inline
gets no ticket.

Fixes land in the **source** — the `.rs` header or the `docs/modules/*.md` that
composes the page. Never edit `target/doc/`; it is a build artifact and the next
`just docs` overwrites it. Rebuild and re-dump to confirm a page-level fix.

Report a per-page verdict — `ok`, `fixed`, `drifted`, or `unverifiable`, where
`fixed` means all drift repaired inline and `drifted` means a ticket is open. A
page where you did both is `drifted` — an open ticket outranks a partial fix.
Then state checked-of-total against the denominator you enumerated, so a partial
sweep is never mistaken for a complete one.

**Commit the whole sweep as one commit**, not one per file. The repo convention
is one conventional commit per slice, and a sweep is one slice. List the files
you changed in the body, one line each with the claim you corrected, so a
reviewer can map every hunk to a finding without re-deriving it.

## Verifying afterwards

Whatever you changed, run `just lint`. It must be green with no pre-existing
failures to excuse. It gates `cargo rustdoc -- -D warnings` (so an unclosed
HTML tag or a stray untagged code fence is an error, not a warning you have to
grep for) and the suite, which runs `tests/docs.rs` — the mechanical check
on `//!` header shape, summary length, and pointer targets. Both are stricter
than reading the output yourself; do not substitute a narrower command.

`just lint` is safe to run as an agent: it delegates its test step to `just
test`, which loops the targets one rustc at a time rather than issuing the bare
`cargo test` that `qp-implement` forbids (QP-167). Run the recipe, not a
hand-rolled subset of it.

If `just lint` fails on something you did not touch, that is a finding: file
it, do not fold the fix into a docs ticket.
