Containment nests, blocking annotates. A wave prints with its slices indented
beneath it, and each row's `<- [...]` names only what is genuinely stopping it.
Before edge modes existed both rendered as the same flat `<- [...]` list, so a
wave read as blocked by the slices it is made of.

The output is one line per task, never a subset: nesting decides how rows are
*arranged*, and the filters decide which rows exist. A slice whose container was
filtered out by `--tier` therefore prints at the top level rather than
disappearing with its parent, and a slice sitting in two containers prints once,
under the first.

## Ordering and cycles

Roots are the tasks with no container inside the rendered set. Everything else
is reached by walking `contains` edges down from a root, so a task's depth is
its containment depth. The walk carries a `seen` set, which serves two purposes:
it makes multiple containment single-print, and it bounds the recursion even if a
containment cycle ever slipped past the write path's check. A renderer that hangs
is worse than one that shows a task under only its first container.

## JSON

`--json` is flat — it is data for a caller that will do its own layout, so
nesting would only get in the way. Each task carries three edge lists in
display-id form: `blocked_by`, `contains`, and `part_of`.

`depends_on` predates them and keeps its original meaning, every dep edge in
both modes, as raw integer ids. It is redundant with the other three and stays
only so an existing consumer is not broken by the split.
