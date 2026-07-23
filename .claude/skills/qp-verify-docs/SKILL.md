---
name: qp-verify-docs
description: Verify documentation against the code it describes, then fix what drifted or file it as a ticket. Use when a doc statement looks wrong while reading, or to sweep all docs for staleness. Invoked as `/qp-verify-docs <file>[#<anchor>]` or `/qp-verify-docs --sweep`.
---

## Why this exists

quipu's prose documentation is written by an LLM about code the owner cannot
fully audit. That is a real risk: a confident, wrong doc is worse than no doc.
This skill is how a claim gets challenged.

## When to invoke

Two modes, and they are not interchangeable:

- **Targeted** — a human noticed something wrong while reading and copied a
  command. A *reporting shortcut*.
- **Sweep** — systematic drift detection across all documented files. The
  actual *detector*.

A targeted run proves nothing about the rest of the docs. Do not report "docs
verified" after one.

```
/qp-verify-docs docs/architecture.md#three-concepts   # one section
/qp-verify-docs src/cmd/claim.rs                      # one file's //! header
/qp-verify-docs --sweep                               # every documented file
```

Read anything the caller typed after the argument; it is usually the specific
complaint and the most valuable input you have.

A targeted run names a source file, but the page is still where you look for
anything beyond the single claim — build the rendered page and dump it (see
`references/source-vs-rendered.md`).

**Resolving `#<anchor>`.** Anchors are rustdoc heading slugs: lowercased heading
text, non-alphanumerics collapsed to `-`. `#three-concepts` is
`## Three concepts`. Find the matching heading in the file and verify from there
to the next heading of the same or higher level. If the anchor matches nothing,
say so and verify the whole file rather than guessing.

## The core loop

Work claim by claim, whether targeted or sweeping:

1. **Build the docs** — `just docs` / `just docs-dump`, then read the rendered
   page with `w3m`, not raw HTML. A sweep reads the *page*, not the source file.
2. **Enumerate the denominator** — in a sweep, count the pages first with
   `find target/doc/qp -name 'index.html'`; never trust a count you were handed.
3. **For each claim, classify** — behavioural, structural, or rationale. This
   decides how you verify it.
4. **Verify** — trace a behavioural claim to a test or a runnable command, a
   structural claim by reading the code, rationale against `qp decisions` and
   the vault.
5. **Keep, fix, or cut** — a correct sentence still has to earn its maintenance.
   Fix in the source (`.rs` header or `docs/modules/*.md`), never in
   `target/doc/`.
6. **Fix or ticket** — fix in place when the wording is recoverable without
   judgement; file a ticket when it takes a choice. Say nothing when the claim
   is correct.

## References

Load a reference file with Read when you reach the step it covers:

- Classifying a claim (behavioural / structural / rationale, and the
  `$QUIPU_VAULT`-unset case) → `references/classification.md`
- Deciding keep/fix/cut, the four durable kinds, where the prose lives, and
  fix-in-place vs file-a-ticket → `references/keep-fix-cut.md`
- How the prose should read (House style, before/after) →
  `references/house-style.md`
- Source vs rendered page, and the `just docs` / `w3m` / `find` / `ripgrep` /
  smart-typography mechanics → `references/source-vs-rendered.md`
- Running a full sweep (denominator, per-page checks, verdicts, one commit) →
  `references/sweep.md`

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
