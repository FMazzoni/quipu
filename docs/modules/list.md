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
