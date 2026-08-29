A served verb's transaction can move tasks the request never mentioned:
finishing a blocker promotes everything it was holding back, and raising one
freezes everything under it. Locally that omission is invisible, because the
caller's next read hits the same file and sees the truth. Across a wire the
store is still right and the caller is still never told, so an orchestrator
that dispatches on "what just became ready" has to poll for an answer it could
have been handed — which is a large part of why `wait` and `watch` exist.

So every payload whose transaction writes a `state_change` for a task the
caller did not name carries those changes in a reserved `_effects` key.

## Why the key is reserved, and why it is a list

The leading underscore marks the key as belonging to the *envelope* rather than
to the verb, so a verb author naming a field can never collide with one. That
is not hypothetical: the obvious unprefixed name, `promoted`, is already taken
by `depends` and already means something narrower.

`depends.promoted` is the precedent, and it is the shape not to copy. It takes
the full `Vec<i64>` a sweep returns and reduces it to
`promoted.contains(&task_id)` — a bool about the *named* task. That is a report
of something else that happens to fall out of the same call, not a partial
report of collateral, and the two are kept apart deliberately: `_effects`
excludes every task the request named, so the bool and the list never describe
the same task and neither can be derived from the other.

## Present iff non-empty

Absent, not `[]`, when nothing collateral moved. `#[serde(default,
skip_serializing_if = "Vec::is_empty")]` is what gives that, and `default` is
load-bearing rather than tidy — a payload with no collateral omits the key, and
without `default` a consumer cannot read back a payload the producer considers
perfectly valid.

The key is on the CLI's `--json` too, not only on a wire. One shape, not a
wire-only dialect; it is additive, so nothing that reads the payload today
breaks.

## Rowids stop here

The sweeps in `db.rs` deal in rowids, because that is what a guarded `UPDATE`
touches. A rowid names a row in the store that minted it and means something
else — or nothing — in any other store, so `collateral` is the one place that
turns them into `display_id`s. Eight verbs feed it; a `SELECT display_id` in
each of them would be eight chances to put a rowid on the wire.

## Boundary

`human()` does not read this. `_effects` is a serialised-form field, and the
prose lines are unchanged by it. Whether a promotion belongs in the sentence is
a real question and a reasonable later change — but if it is made, `human()`
must read this field rather than recompute anything, or the two renderings
drift apart, which is the failure `Outcome` exists to prevent.
