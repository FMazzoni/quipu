The general read surface over the `event` table. Every other event-shaped
command is a narrowing of this one: [`decisions`](../decisions/index.html) is
`timeline` with `kind = 'decision'` and an `--auto-only` predicate,
[`watch`](../watch/index.html) is `timeline` polled in a loop. All three go
through one `store::EventFilter` rather than growing parallel queries, which is
what stops them drifting apart on the semantics below.

## `--since`

`--since` is an event id, not a timestamp. `--since 730` means "events with
`event.id > 730`" — an **exclusive** lower bound on a monotonic integer, so it
returns events starting at 731. It does *not* accept `24h`, `7d`, or a date.

The neighbouring `report --since` **does** take a duration or a date, and the two
flags share a name while meaning different things. The asymmetry is not an
oversight: `report` produces a human-facing snapshot where "the last day" is the
natural window, while `timeline` is a cursor-driven tail where the caller's
question is "what happened after the last thing I saw". An id answers that
exactly and a timestamp cannot, because two events written in the same second are
ordered by id and not by `ts`.

Ids are safe to use as a cursor because they are gap-free as readers see them:
events are only ever inserted inside `db::with_tx` (`BEGIN IMMEDIATE`), so no
reader observes an id that is still uncommitted. That is the same invariant
`watch` relies on.

Omitting the flag leaves `since_id` as `Some(0)`, not `None` — a lower bound that
happens to admit everything because ids start at 1. `decisions` passes `None`
here instead; the results coincide today, and the difference is only visible to
someone reading the generated SQL.

## Repeated `--kind` flags

Repeated `--kind` flags compile to `kind IN (?, ?, …)`, so
`--kind decision --kind blocker` returns events that are **either**. This is the
opposite of `qp list --tag`, where repeated flags AND together and a task must
carry every one.

Both are the useful reading of their own domain — an event has exactly one kind,
so ANDing them would always return nothing, while a task has many tags and
narrowing is the point — but the two commands sit next to each other in the same
CLI, so the divergence is worth stating rather than discovering.

Rows come back in `event.id ASC` order, oldest first, always. There is no reverse
flag and no limit: `timeline` hands over the whole matching range and leaves
windowing to the caller's cursor.
