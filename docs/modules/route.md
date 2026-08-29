# `route`

The served-verb table, expressed as data so that adding a verb is a line in a
list rather than an arm in a match.

## The merge shape this avoids

`main.rs` already carries two one-line-per-verb match tables. A fan-out slice
that adds five verbs by inserting five adjacent single lines into a thirty-arm
match, from a branch, collides with any other branch doing the same — and the
conflict is textual noise around semantically independent changes. That has
already happened in this repository.

So the shape here is: one `Served` impl beside each command, one line in the
`served!` list, and generic dispatch. `handle::<S>` is the server's handler for
every verb, `dispatch::<S>` is the CLI's arm for every verb, and neither grows
when the set does. The list generates both `ROUTES` (what the front door
answers) and `is_served` (what the client sends over the wire), which is the
part that matters: those two must name the same set, and generating them from
one list is the only way they cannot drift.

## What a fan-out slice does

1. Split the command's `run` into an `exec` that returns its `Outcome` and
   leave the `emit` to `dispatch`.
2. Derive `Serialize, Deserialize` on its args, `#[serde(skip)]` the `json`
   field, `#[serde(default)]` anything optional.
3. Add the `Served` impl.
4. Add one line to `served!` and flip that verb's arm in `real_main` from
   `cmd::<v>::run(&db_path, a)` to `route::dispatch::<...>(&ctx, a)`.

Nothing else anywhere changes.

## `--json` does not cross the socket

It is `#[serde(skip)]` on every args struct, and `wants_json` is read off the
args before they are consumed. The flag is a property of the invoking terminal;
putting it in the protocol would make the response depend on how the caller's
tty was going to render it. The server returns the structured payload and the
client renders `human()` from it — which is why `Served::Out` is bound by
`DeserializeOwned` as well as `Outcome`.

## `deny_unknown_fields`

Every served args struct sets it. A typo'd field in a hand-written request is
then `invalid_input` (400) with the field named, rather than a silently ignored
argument and a request that did something other than what was asked.
