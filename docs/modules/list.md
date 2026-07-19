Both free-text filters — `--assigned-to` and `--tag` — are SQLite `GLOB`
patterns, and repeated `--tag` flags AND together (a task must carry a
match for every one). A pattern with no wildcard is an exact match, so
every invocation written against the older exact-match `--tag` keeps
working. See `store::TaskFilter` for why both predicates share one
matching language and why literal `[`/`]` are not escapable.
