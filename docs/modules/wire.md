# `wire`

The two functions that attach an out-of-band discriminator to the one JSON
grammar quipu already speaks, and the one that takes it off again.

There is exactly one error grammar in this system — the `{"error": {...}}`
envelope `main.rs` writes to stderr under `--json` — and this module does not
introduce a second. `failure` pairs that same body with an HTTP status;
`error_from` throws the status away and rebuilds the typed error from the
body's `kind`.

## Why the status is not the taxonomy

`kind` is the contract. The status is a lossy projection of it, for proxies,
`curl` and load balancers, and the projection is deliberately not injective:
`not_found` and `no_store` are both 404, `conflict` and `not_owner` are both
409. Inverting it would collapse exit 6 into exit 2 — "this machine is not
attached to that project", which must never be retried, would become "the store
looked and said no", which a wave orchestrator retries on sight. So
`error_from` reads `kind` from the body and uses the status for one thing only:
recognising a gateway answer that quipu itself is forbidden to send.

## The property this module exists to protect

An existing skill's exit-code branch keeps working with a server in the path. A
conflict is exit 2 whether the store was on this disk or behind a socket. That
is asserted directly by `the_status_map_is_the_documented_table`, which walks
every row of the contract's table in both directions and checks the exit code
that comes out the far end.

## 503 is reserved

`unavailable` means *no store saw the request*. A 503 written by a quipu
process would be proof that a front door answered, contradicting the kind, so
`status_for` has no row that produces one and the server never sends one. When
a `502`/`503`/`504` does come back it can only have been written by something
in between — a reverse proxy — and that is exactly `unavailable`, exit 7.

## The leak

`QuipuError::Conflict` and `Invariant` store `code` as `&'static str` because
every construction site in the tree passes a literal. A code arriving off the
wire is a `String`, and the codes are an open set the contract says may grow
additively, so there is no closed table to intern against. `intern` leaks it.
It is bounded by construction — a client process issues one request and exits —
and the alternative, widening `code` to `Cow<'static, str>`, rewrites every
error site in the tree to reclaim a byte that `exit` already reclaims.
