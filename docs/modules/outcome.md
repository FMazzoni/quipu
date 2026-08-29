This module is the success half of the CLI's output contract. Errors are
rendered once in `main.rs` as an `{"error": ...}` envelope on stderr; `Outcome`
is the matching single rendering point for everything that succeeds, on stdout.
Every mutating command's result type implements it, plus `init` and `remote
doctor`. The exact set is whatever the tree declares rather than a number
written down here, because a number written down here goes stale silently: it
said "fifteen" for as long as it took someone to count, and the answer was
nearly twice that. `the_registry_is_every_payload` scans `src/**/*.rs` for
`impl Outcome` and fails if the set it finds and the round-trip registry ever
disagree, so the tree is the count.

## The trait

`Outcome: serde::Serialize` adds one method, `human() -> String`. A command
builds its result struct once, after the transaction commits, and hands it to
`emit(json, &o)`, which prints either `serde_json::to_string(&o)` or
`o.human()`. That is the whole module.

The point is that the two renderings cannot drift apart, because they are two
views of one struct. A command that formatted its prose line inline and built a
`serde_json::json!` literal separately would let `--json` and the human line
disagree about what happened, and nothing would catch it.

## No `ok` wrapper

The JSON is the bare object — `{"display_id":"QP-1",...}`, not
`{"ok":true,"data":{...}}`. Success is already disjoint from failure twice
over: by stream (stdout vs stderr) and by exit code. A consumer that reads
stdout on exit 0 does not need a discriminant, and a wrapper would cost every
caller a level of indirection to reach the fields it wanted.

## What belongs in the struct

Two conventions the code follows and a reader should keep:

- Carry `display_id`, not the raw argument the user typed. `id::resolve_full`
  returns the store's canonical form for exactly this purpose, and `human()`
  echoes it.
- Include the resulting `state` where a state-machine edge was traversed, so a
  `--json` consumer can confirm the edge landed without a follow-up `qp show`.

`#[serde(skip_serializing_if = "Option::is_none")]` on optional fields keeps
absent values out of the JSON rather than emitting `null` — see `add.rs`'s
`Created`. It must be paired with `#[serde(default)]`; see below.

## Payloads round-trip

Every payload derives `Deserialize` as well as `Serialize`, and is at least
`pub(crate)` rather than private to its command module. That is not decoration.
The output contract's other half is that prose is rendered from the payload and
never carried alongside it — so a caller holding only the JSON must be able to
rebuild the struct and call `human()` on it and get the same sentence. A
private, serialize-only struct makes that impossible, and the cost of finding
out later is a sweep of every command module at once.

The property is `human(deserialize(serialize(o))) == human(o)`, asserted by
`assert_round_trips`. It is asserted rather than argued because it is not
obvious which changes break it:

- `skip_serializing_if` omits the key entirely, so the field needs
  `#[serde(default)]` or deserialisation fails on a payload the producer
  considers perfectly valid. The failure is invisible until something reads the
  JSON back.
- `&'static str` cannot be deserialised at all. Several payloads carried one
  for a fixed discriminant (`op`, `mode`, `identity`, `source`); they carry
  `String` now.

`every_payload_round_trips` exercises each payload's `Default` value, which is
deliberately the adversarial shape: every `Option` is `None` and every `Vec` is
empty, so every `skip_serializing_if` key is absent. The populated shapes fail
in the opposite direction, so a handful of command modules assert their own —
`take`'s `Some` arm, `add`'s populated optionals, `remote`'s flattened
`Checked`, `sync`'s embedded outcomes, `doctor`'s non-empty `checks`.

Adding a payload without any of this fails `the_registry_is_every_payload`,
which compares the source scan against the hand-written registry. Rust has no
reflection, so one half is written by hand; scanning for the other half is what
keeps the hand-written half honest. Same technique as `tests/docs.rs`'s
`POINTERLESS_MODULES` and `store.rs`'s signature scan, and for the same reason:
a checklist in a comment does not fail a build.

## Boundary

`emit` handles success only. It does not touch exit codes, stderr, or the error
taxonomy; those are `db::QuipuError` and `main.rs`. Read-only commands
(`list`, `show`, `tree`, `timeline`, `report`, `status`) do not use `Outcome` —
their output is a projection of the store rather than the result of a mutation,
and they serialize their own shapes.
