Convenience wrapper. Equivalent to:

```text
qp add "<new>" --tag kind:blocker
qp depends <task> --on <new-id> --as <agent>
qp abandon <task> --as <agent>
```

collapsed into one transaction so partial failures can't leave a dangling task.

WHY `--tag` EXISTS: `kind:blocker` is a skill-layer convention, not substrate
truth, and CLAUDE.md forbids baking orchestration patterns into the binary. It
survives only as the *default* of `--tag`, so the one-liner documented in
`skills/wave/SKILL.md` keeps working while a caller with a different taxonomy
passes its own. Repeat `--tag` for several. Passing any at all *replaces* the
default rather than adding to it — a caller naming its own taxonomy does not
want a foreign one silently merged in.

WHAT IS DELIBERATELY NOT OVERRIDABLE: the `blocker` event kind. That names the
operation the binary performed — a sibling of `state_change` and `dep_added` —
and `cmd::render` switches on it to pull `title` out of the payload. It is
substrate vocabulary, so it stays fixed; only the tag was ever the pattern.

WHY OWNERSHIP IS FOLDED INTO THE `WHERE`: the demotion's guard checks the state
*and* the caller's open assignment in one `UPDATE ... WHERE ... AND EXISTS`,
rather than verifying ownership in a separate read first. That keeps the mutation
a single source of truth, per the guarded-transition contract — a separate check
would be a read-then-write with a window between them.

The cost is that a failed `UPDATE` says only "zero rows", which cannot tell
wrong-agent from wrong-state, and those want different reactions: one is a
permanent `not_owner`, the other a `conflict` a caller might retry. So the
failure path does read the task back — deliberately *after* the guard has already
decided, for diagnosis only and never for control flow.
`block_wrong_agent_yields_not_owner_not_conflict` and
`block_wrong_state_yields_conflict_not_owner` pin both directions, which is what
stops that reporting logic from being "simplified" into a single error.

BOUNDARY: nothing in the binary reads the tag back. `cmd::wave` classifies a
task as blocked from unresolved dep edges alone, which is why choosing a
different tag cannot desync the wave view. The tag is purely a `qp list --tag`
filter handle for whatever skill is driving.
