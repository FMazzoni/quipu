Polling. Relies on the watch-correctness invariant: events are only
inserted inside `db::with_tx` (IMMEDIATE), so `event.id` is gap-free as
seen by readers — `WHERE id > last_seen` is safe.

## A resolved front door is a refusal, not a local tail

`watch` is the same shape as `wait` and the consequence is worse. With a front
door resolved it printed a warning and then streamed the *local* store's events
on stdout, indistinguishable from a real tail of the server's log. So it
refuses instead: exit 1, nothing on stdout, naming the front door it declined
to answer for. `docs/modules/wait.md` carries the full reasoning; it applies
here unchanged, and `route::is_client_poll` names both verbs in one place.
