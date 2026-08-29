Unlike deps, relations never affect readiness — they are provenance, not
scheduling. The `kind` is free-form: `add` rejects only an empty string, so
`variant-of`, `supersedes` and `fixes` are conventions from `skills/`, not a
vocabulary the binary knows. `add` and `rm` are
idempotent: the outcome reports whether a row actually changed, and the
`relation_add`/`relation_removed` event is only written when one did.

## One route, three ops

`relation` is served as a single verb: the op is a field in the request body
rather than a path segment, and the response is an `untagged` enum over the
three payloads. The reasoning — why a subcommand group cannot be split across
the local/served boundary, and why `untagged` rather than a wrapper — is in
`docs/modules/route.md`.

The consequence to know when reading the code: `RelationListed` renders prose
but is deliberately not an `impl Outcome`. The registry in `outcome.rs` is the
set of payloads that cross the wire on their own, and this one only ever travels
inside `RelationOutcome`.
