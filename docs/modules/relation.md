Unlike deps, relations never affect readiness — they are provenance
(`variant-of`, `supersedes`, `fixes`), not scheduling. `add` and `rm` are
idempotent: the outcome reports whether a row actually changed, and the
`relation_add`/`relation_removed` event is only written when one did.
