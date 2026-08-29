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

## The read side: one endpoint per command

The read verbs do not use `Outcome`. They build their own projections at the
call site and print them, which is why fronting them is a design job and the
write fan-out is not.

**The problem is round-trip amplification.** Building `report`'s board is seven
reads of the store; `report --all-tickets` is six *per ticket* and unbounded;
`show` is ten; `tree` is four to six. A protocol that exposed the read layer
query-by-query — `/v1/store/tasks`, `/v1/store/events`, `/v1/store/deps` — would
compose beautifully and turn every one of those numbers into that many round
trips. Over a link whose latency dwarfs a SQLite read, that is the whole cost of
the view.

So: **one endpoint per command, and the same `Served` machinery the writes
use.** The wire shape mirrors the CLI, the round trip count is one, and it is
one *structurally* rather than by discipline — `handle::<S>` is a single `exec`
against a single per-request connection, and there is no seam through which a
second call could appear. `each_read_is_exactly_one_round_trip` in
`tests/serve.rs` fails if a later change opens one.

What that costs, stated plainly so the trade is visible: **the server grows an
endpoint per view, and a new view is a protocol change.** That is the right side
of the trade here because the views are few, slow-moving, and each is a
*question an operator asks* rather than a composable primitive. It would be the
wrong side for a general query API, and if quipu ever wants one it should be a
second grammar under a second prefix, not these endpoints decomposed.

A third answer was considered and rejected: one batch endpoint taking a list of
read requests. It keeps the round trip at one and needs no endpoint per view —
but it puts the composition in the *client*, so every caller re-derives what a
view is, two callers drift on it, and the server can no longer answer "what does
`report` mean" at all. It is the query-by-query design with the latency hidden.

### The payloads are the projections

Each read gets a view type: a `#[serde(transparent)]` newtype over the
`serde_json::Value` the command already built. Two consequences worth being
explicit about, because a reader will reach for typed structs instead:

- **The response body is byte-for-byte what `--json` already printed.** A
  consumer that piped `qp list --json` into `jq` reads the same bytes off the
  socket. Typed structs would have re-derived those bytes and a `json!` map
  orders its keys alphabetically where a derive emits them in declaration order
  — so the switch would silently rewrite every existing consumer's input.
- **`human()` renders from the payload, as `list` and `wave` already did.** Both
  read their own `Value` to print prose today; the read side did not acquire
  that shape, it always had it.

This is why the read-layer row types needed no derives. `store::TaskRow` and
`store::EventRow` are still bare structs: they are the *input* to a projection,
never the payload. The one derive this slice did add is `Serialize` +
`Deserialize` on `db::State`, because `list --state` is a served filter and the
same word has to name the same state in argv, in the column, and in a body.

### Filters cross the wire; results do not get trimmed

Every filter is a field on the args struct, so it is in the request body, so it
is a SQL clause on the server. `--state`, `--tag`, `--assigned-to`, `--kind`,
`--since`, `--auto-only`, `--wave`, `--ticket` and the positional task all
narrow the answer before a byte is written.

That is not tidiness, it is what makes `wait` and `watch` affordable. Both are
500 ms polls of these endpoints, and both poll a *narrowed* one: `wait` watches
a cohort that is by definition small and shrinking, and `timeline --since <id>`
answers a caught-up poller with an empty array. Neither needs an endpoint of its
own, and neither should get a streaming one — that wants an async runtime.
`filters_are_applied_before_the_response_is_written` asserts this against raw
response bodies, because a client that fetched everything and trimmed would
produce identical *output* while moving the whole store per poll.

`report --since` is the one filter whose argument is interpreted rather than
matched: `24h` resolves against the **server's** clock, in `exec`. That is
right — it is bounding the server's own `event.ts` values, which are UTC — but
it is worth knowing before debugging a window that looks off by a clock skew.

### What stays on the client

`--json` (a property of the terminal), `--with-description` and `--show-tags`
(how many lines `human` prints), and `report --output` (a path on the caller's
disk). The first three are `#[serde(skip)]` and the last two rendering flags
come back through `Served::restore_view`; `--output` is why `report` calls
`route::fetch` and writes the file itself instead of going through `dispatch`.

The rule they share: **the response body must not depend on how the caller was
going to render it.** A server that echoed `with_description` would make two
invocations differing only in a terminal flag produce different bytes.

### `relation list` is not a read-side endpoint

It is served, but by the row above rather than by this slice's shape, and the
next section says why. The read side briefly split the group — `relation list`
as its own args struct, plus an optional `if <path>` guard on `served!` so
`is_served` would not claim the unserved ops — because from where it sat only
`list` was servable. The write fan-out then served the whole group, which
removes the hazard the guard existed for, so both the split and the guard are
gone. An unused mechanism with a subtle purpose is worse than no mechanism.

The count in this file's read-side section is eight views, not nine, for the
same reason: `relation` is one route for three ops.

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
