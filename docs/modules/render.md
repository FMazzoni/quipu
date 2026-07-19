`summarize_payload` is called by `timeline` and `show`, so that one event kind
reads the same wherever it appears.

The reason to centralise this is drift, not reuse. Two views formatting
`state_change` independently will eventually disagree about it, and a reader
comparing a task's `show` output against the global timeline would have to work
out whether the difference means anything. It never does.

Unknown kinds are the interesting case. Rather than erroring or printing nothing,
the fallback dumps the raw JSON payload truncated to 80 characters — so an event
kind added to a mutator but not yet taught to this function still shows up as
something a human can read, and a stale renderer degrades instead of hiding
history. That is the right default for an audit log: the log is authoritative,
this is only a lens over it.

Truncation counts `char`s, not bytes. Slicing a UTF-8 string on a byte boundary
panics, and event payloads carry arbitrary user text — task titles, decision
notes — so this is a live crash path, not a theoretical one. Pinned by
`payload_summary_survives_multibyte_char_at_truncation_boundary`.

Every field access is a lookup-with-fallback rather than an unwrap, for the same
reason: payload shapes come from disk and may have been written by an older
binary. A missing field renders as empty; it never takes down the command that
was only trying to show you what happened.
