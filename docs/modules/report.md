The synopsis below is fenced deliberately: outside a code block, rustdoc parses
`<id>`, `<duration>` and friends as unknown HTML tags and silently deletes them,
losing every argument placeholder (QP-96).

```text
Modes:
  default            board payload: { tasks, events, deps }
  --ticket <id>      full detail for one ticket (parents, children, uncapped events)
  --all-tickets      JSON array of the same per-ticket detail, one per ticket in scope

Scope filters:
  --since <duration> filter events: 24h, 7d, or RFC3339 date
  --wave  <task-id>  scope to the dep subtree of the given task

Output:
  stdout by default, or --output <path> to write to a file.
```

## Rendering lives elsewhere

Markdown and HTML rendering used to live in this binary. It now lives in the
`skills/report-render/` skill — see that skill for the section structure (state
snapshot, in-flight, timeline, friction log, open bugs, shipped) that an agent
reconstructs from this JSON.
