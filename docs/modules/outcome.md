This module is the success half of the CLI's output contract. Errors are
rendered once in `main.rs` as an `{"error": ...}` envelope on stderr; `Outcome`
is the matching single rendering point for everything that succeeds, on stdout.
Fifteen types implement it, across every mutating command plus `init` — adding
a mutating command means adding a sixteenth.

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
`Created`.

## Boundary

`emit` handles success only. It does not touch exit codes, stderr, or the error
taxonomy; those are `db::QuipuError` and `main.rs`. Read-only commands
(`list`, `show`, `tree`, `timeline`, `report`, `status`) do not use `Outcome` —
their output is a projection of the store rather than the result of a mutation,
and they serialize their own shapes.
