Both free-text filters — `--assigned-to` and `--tag` — are SQLite `GLOB`
patterns, and repeated `--tag` flags AND together (a task must carry a
match for every one). A pattern with no wildcard is an exact match, so
every invocation written against the older exact-match `--tag` keeps
working. See `store::TaskFilter` for why both predicates share one
matching language and why literal `[`/`]` are not escapable.

## The `agent` column

The `agent` column is the last assignee, not the current one. It comes from
`store::LATEST_AGENT_SUBQUERY`, which takes the most recent
assignment row regardless of whether it is still open. So a task that has been
abandoned or reclaimed still lists the agent that dropped it, and `--assigned-to`
will still match it. This is not a bug and the state column is what disambiguates
— a `ready` row with an agent name means "was worked on, now free". The other
question ("who holds this right now") is `db::current_assignment`, and the two
must not be swapped: `depends_uses_latest_open_assignment_not_latest_by_id`
exists because an ownership check that used this one would let a former assignee
keep authority it no longer has.

## Bulk enrichment

Tags, blocked-by lists and last-event are fetched for the whole selected id set
in one query each, rather than per row. The row-at-a-time shape is the obvious
one to write and turns a listing into an N+1; keeping the helpers in `store.rs`
bulk-shaped means the obvious call is also the fast one. They chunk their
`IN (...)` lists under SQLite's variable limit, which is theoretical at current
scales and free to keep correct.

`--state` is parsed as `db::State` rather than a string, so a misspelling is
rejected at parse time instead of silently matching zero rows and looking like an
empty project (`list_state_rejects_invalid_spelling_at_parse_time`).

## The stale marker

A hold whose lease has run out trails its row as
`[STALE — quiet since <ts>, on <host>]`, after TITLE. It is **not** a sixth
column, and that is the deliberate half: `show`'s header line is pinned to
`list`'s columns in `list`'s order by
`show_header_mirrors_list_columns_and_tier_is_labelled`, so a column added here
is a positional change every reader of both views inherits, and it is exactly
the shape that made a `-` read as "no agent" once already. TITLE is last, so
trailing text moves nothing. `wave` reached the same conclusion about its own
four columns, and both render `lease::stale_marker` so the two board views
cannot describe one stranded ticket differently.

`--json` carries the same facts without the layout question: `stale` beside the
row and the whole hold object under `hold`, spelled exactly as `wave --json`
spells them.

Liveness is fetched by `lease::holds_by_task` — one statement for the page,
alongside the other bulk enrichments — and it embeds the same `LIVE_HOLD`
predicate `reclaim`'s guarded `UPDATE` does. That is the property to keep: what
a reader is told is stale and what `reclaim` is willing to break are one
question, not two that can drift. A hold that stated no expiry (no
`lease_seconds`, or no `last_contact` — the shape every claim written before
leases existed is in) is never stale, so it never carries the marker.
