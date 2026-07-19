Polling. Relies on the watch-correctness invariant: events are only
inserted inside `db::with_tx` (IMMEDIATE), so `event.id` is gap-free as
seen by readers — `WHERE id > last_seen` is safe.
