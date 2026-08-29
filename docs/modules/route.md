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

## A subcommand group is one route, never several

`Served` is implemented for an args *struct*, and `is_served` matches on a `Cmd`
*variant*. Both are the group, not the op: there is no `Cmd::RelationAdd`, so
`relation` and `tag` are served whole or not at all.

That is not a limitation to work around — it is the same rule the wire contract
applies to `remote`, arrived at from the other side. Serving `relation add` and
leaving `relation list` local would send the write to the front door and run the
read against whatever store is on the *client's* disk, which for a caller
pointed at a server is usually no store at all. The op therefore travels inside
the request body (`{"op":{"add":{…}}}`, lower-cased to match what the caller
types), and the response is one `untagged` enum over the payloads the ops
already had — untagged so each variant stays the bare object it has always been
on the wire, rather than every existing `--json` consumer gaining a wrapper.

The cost is that a group is served only when *every* one of its ops can be. That
is a real constraint on which slice may serve a group, and it is why `relation`'s
read op landed with its writes rather than with the other reads.
