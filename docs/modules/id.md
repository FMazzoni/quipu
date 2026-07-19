This module owns the `<PREFIX>-<rowid>` display id (JIRA-style: `QP-1`,
`ACME-42`) and the translation in both directions. The prefix is per-store,
2–5 uppercase letters, fixed at the first `qp init` and ignored by later ones;
`QP` is the default.

## Rowid is the identity, display id is a label

`display_id` is a `NOT NULL UNIQUE` column on `task`, but nothing resolves
through it. `parse` extracts the digits and `resolve_full` looks the row up by
`WHERE id = ?` — the integer rowid. The prefix a caller types is validated for
shape and then discarded.

That is deliberate, and it is what makes the CLI forgiving in a way a string
match could not be. `QP-1`, `qp-001`, `  qp-1  ` and `ACME-1` all reach the
same row, because none of the differences between them survive `parse`
(`resolve_handles_whitespace_and_lowercase`, `resolve_handles_zero_padding`).
An agent that reconstructs an id from a log line, a shell variable, or its own
memory of the prefix gets the right task or a clean `not_found`, never a
silently different one. `parse` also still accepts the legacy `T<DIGITS>` form.

The cost is that a prefix typo is not caught. `ACME-1` against a `QP` store
resolves to `QP-1` rather than erroring. Cross-store id confusion is guarded
elsewhere — see the project-uuid mismatch warning in `db.rs` — not here.

## `resolve` and `resolve_full`

`resolve_full` returns `Resolved { id, display_id }`; `resolve` is a wrapper
that discards the second field. Both run the same query.

Callers that print an identifier back to the user must use `resolve_full` and
echo `display_id`, never the argument they were given — otherwise a user who
typed `qp-001` sees `qp-001` in the output and in `--json`, and the store's
canonical form stops being canonical (`resolve_full_returns_canonical_display_id`).

`resolve` is kept on purpose as the rowid-only entry point, not as leftover
scaffolding. QP-113 asked whether to migrate the remaining callers and delete
it, and resolved to keep it: `tree`, `watch`, `add` and `timeline` filter or
join on the rowid and never print an identifier, so forcing them through
`resolve_full` would allocate a `display_id` `String` that is dropped
immediately. Use `resolve` when no id is printed, `resolve_full` when one is.

## Not-found is part of the taxonomy

`resolve_full` maps `QueryReturnedNoRows` to `db::not_found`, carrying the
user's trimmed input as the subject so the message names what they typed
(`resolve_missing_task_echoes_raw_input`). This is the single site that raises
`not_found` for a task reference; every command inherits exit code 2 and the
`{"error":{"kind":"not_found"}}` envelope from it without writing any handling
of its own. The only other `not_found` in the crate is `depends.rs` rejecting a
missing *edge*, which is a different subject.
