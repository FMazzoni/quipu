Format: `<PREFIX>-<rowid>` (JIRA-style), e.g. `QP-1`, `ACME-42`. The prefix is
per-store, fixed at `qp init`, default `QP`. `parse` accepts any
`<LETTERS>-<DIGITS>` form, plus legacy `T<DIGITS>` for one release of grace.
`resolve`/`resolve_full` match on the parsed rowid (not the display string),
so the prefix in the input is informational and zero-padding (`QP-001`) is
accepted — what matters is that a row with that id exists.
