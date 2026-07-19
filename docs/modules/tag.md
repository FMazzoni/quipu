Tag *values* are free-form on purpose — the binary stays ignorant of what
`kind:decision` or `decision:critical` mean, because orchestration patterns live
in `skills/`, not here (CLAUDE.md). `add` therefore validates shape and never
vocabulary: it rejects an empty name and a name ending in `:`, and nothing else.

The trailing-colon rule exists because the `prefix:value` namespace convention
is load-bearing for lookup — `qp list --tag commit:<sha>`, `--tag "decision:*"`
— and a bare `prefix:` asserts a namespace while supplying no member, so it can
never be the answer to any of those queries. It is not hypothetical: a
`qp tag $T add "commit:$(git rev-parse ...)"` whose substitution ran in the
wrong cwd wrote a live `commit:` row, and the wave-orchestrate Phase 7 audit
(which greps `commit:[0-9a-f]*`) matched it with zero hex digits and reported
the ticket as correctly tagged. That silent false green is what this guard buys
off. The pre-existing empty-name check could not catch it, because it only sees
a substitution that *is* the whole tag, never the far commoner one that is its
suffix.

GOTCHA: the guard is on `add` only. `rm` must keep accepting any string,
including the malformed ones earlier versions admitted — guarding both would
make already-stored bad rows permanently unremovable.

Both ops are idempotent: the outcome reports `added`/`removed` only when a row
actually changed, and the `tag_added`/`tag_removed` event is written only then.
