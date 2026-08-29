The prefix is part of the identifier, not decoration: it is the only
redundancy an id string carries against being applied to the wrong store, so
`parse` surfaces it and `resolve_full` refuses one that is not this store's.
Dropping that check does not fail loudly — it resolves to a real, valid,
*different* row, and every mutating command then changes that row instead.

This module owns the `<PREFIX>-<rowid>` display id (JIRA-style: `QP-1`,
`ACME-42`) and the translation in both directions. The prefix is per-store,
2–5 uppercase letters, fixed at the first `qp init` and ignored by later ones;
`QP` is the default.

## Rowid is the identity, the prefix scopes it to a store

`display_id` is a `NOT NULL UNIQUE` column on `task`, but nothing resolves
through the whole string. `parse` splits a reference into its prefix and its
digits, and `resolve_full` looks the row up by `WHERE id = ?` — the integer
rowid — after checking the prefix against this store's `meta.display_prefix`.

Everything that is *not* identity is still forgiven, which is what keeps the
CLI usable: `QP-1`, `qp-001` and `  qp-1  ` all reach the same row, because
case, zero-padding and whitespace do not survive `parse`
(`resolve_handles_whitespace_and_lowercase`, `resolve_handles_zero_padding`).
The legacy `T<DIGITS>` form parses with no prefix at all and is therefore
unchecked; it predates per-store prefixes and has nothing to compare against.

The prefix itself is identity, and skipping the comparison was a silent data
hazard rather than a convenience. Rowids are dense and start at 1 in every
store, so a given number names a real row nearly everywhere; an id carrying
some other store's prefix used to resolve, be echoed back under *this* store's
prefix, and — for `tag`, `claim`, `complete` and every other mutating command —
change the wrong ticket with no error. The prefix is the only redundancy an id
string carries, so it is checked: a mismatch is `invalid_input` (exit 1) naming
both prefixes (`resolve_rejects_foreign_prefix`).

This is necessary, not sufficient. Prefixes are not unique across stores, so
two projects can share one and a matching prefix still does not prove the id
came from here. Cross-store provenance is guarded elsewhere — see the
project-uuid mismatch warning in `db.rs`.

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
