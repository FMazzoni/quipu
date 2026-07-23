# Sweep mode

Sweeps are per **rendered page**, not per source file. Build and dump the docs
first with `just docs` / `just docs-dump`, and read the dumped pages with
`w3m`, never raw HTML — see `references/source-vs-rendered.md` for the build,
dump, and page-reading mechanics.

**Establish the denominator first.** Enumerate the pages and count them with
`find target/doc/qp -name 'index.html'` — the crate front page plus one page
per module, 32 at this writing. The `find`/`ripgrep` gotchas that make a naive
enumeration over- or under-count (the `target/doc/src/` mirror, per-item pages,
`rg` obeying `.gitignore`) are in `references/source-vs-rendered.md`; get the
count right before you start.

Never accept a page count from the prompt, a previous sweep, or a ticket — the
set grows every wave, and a briefed "23" against an actual 25 turns a 92% sweep
into a reported-complete one. If your enumeration disagrees with the number you
were given, say so and use yours.

Check each page independently; a stale claim on one says nothing about another.

## Page-level checks

These run *in addition to* the per-claim work in `references/classification.md`,
which is unchanged — every sentence on the page still gets classified and
verified. What follows is the set of defects that exist only at page scale:

- **Redundancy across the seam.** The `//!` header and the included markdown
  were written at different times by different agents. Does the header restate a
  section of the markdown? Said twice, a reader trusts neither copy once they
  drift. File this rather than fixing it: choosing which copy survives is a
  judgement call.

- **Commands that do not survive being copied.** Read every command on the page
  as if pasting it into a shell. rustdoc applies smart typography, so an
  unbackticked `--flag` becomes `–flag` and fails silently, and a
  `<placeholder>` can vanish as an unknown HTML tag. Grep the dump for `–`
  adjacent to a word character; numeric ranges (`2–5`, `1–3 ms`) are correct
  typography, not findings. See the smart-typography trap in
  `references/source-vs-rendered.md`.

- **Item-table walls of text.** The module page prints each `///` first
  paragraph untruncated, so one that runs several lines turns the table into
  prose. `tests/docs.rs` rule 7 catches the length; you catch whether the
  summary is a *summary*.

- **Order of answer.** A reader arrives with a question. Does the page answer in
  the order they would ask — what this is, then how it behaves, then why it has
  this shape — or does it open with a caveat about something they have not met
  yet? Resequencing is a ticket; see the structural clause under "File a
  ticket" in `references/keep-fix-cut.md`.

- **Composition gaps.** Does the header's one-line summary still describe what
  the markdown below it covers? The `.rs` summary and the `.md` body drift apart
  because nothing renders them together except this page.

When a finding is visible only on the page, say so in the report. That is the
evidence per-file sweeping would have missed it, and the reason this mode exists.

## Verdicts

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
