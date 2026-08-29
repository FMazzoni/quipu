Polling. Relies on the watch-correctness invariant: events are only
inserted inside `db::with_tx` (IMMEDIATE), so `event.id` is gap-free as
seen by readers — `WHERE id > last_seen` is safe.

## A front door is polled, not answered around

Same shape as `wait`, and the consequence of getting it wrong was worse: with a
front door resolved and no route, `watch` printed a warning and then streamed the
*local* store's events on stdout, indistinguishable from a real tail of the
server's log. It is now a client-side poll over the served `timeline`, through
`route::fetch`, so the same code runs local and served.
`docs/modules/wait.md` carries the full reasoning and the exit-code rule; both
verbs are named in one place, `route::is_client_poll`.

`--since` is an exclusive lower bound on the event id and it is a SQL clause on
the server, so a caught-up poll transfers a two-byte `[]` — measured — rather
than the log. That is what makes tailing cost proportional to what is new.

**Materialised, never streamed.** `timeline`'s rows are a `Vec` before the
response body exists, so no read cursor outlives its response and none can pin a
WAL checkpoint. That is structural rather than a rule to remember: there is no
seam in `handle::<S>` through which a cursor could escape.

The printed line is byte-for-byte what it always was, because `timeline`'s array
elements are `render::event_json` — the same function this module's own
hand-built object was a copy of.
