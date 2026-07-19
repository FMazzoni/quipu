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

BOUNDARY: nothing in the binary reads the tag back. `cmd::wave` classifies a
task as blocked from unresolved dep edges alone, which is why choosing a
different tag cannot desync the wave view. The tag is purely a `qp list --tag`
filter handle for whatever skill is driving.
