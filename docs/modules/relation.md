Unlike deps, relations never affect readiness — they are provenance, not
scheduling. The `kind` is free-form: `add` rejects only an empty string, so
`variant-of`, `supersedes` and `fixes` are conventions from `skills/`, not a
vocabulary the binary knows. `add` and `rm` are
idempotent: the outcome reports whether a row actually changed, and the
`relation_add`/`relation_removed` event is only written when one did.
