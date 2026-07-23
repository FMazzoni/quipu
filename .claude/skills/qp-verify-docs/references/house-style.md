# House style

The keep/fix/cut decision decides *whether* a sentence stays. This decides how
it reads. `docs/architecture.md` is the exemplar — when this section and that
file disagree, the file wins.

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

## Before / after, from the `db.md` rewrite

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
