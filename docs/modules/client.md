# `client`

One blocking HTTP request per invocation, using the `ureq` agent that is
already in the tree for the object-store ledger. No new dependency, and no
async runtime.

## Every failure is the failure the CLI would have produced

That is the whole job. A non-2xx answer goes through `wire::error_from`, which
rebuilds the typed `QuipuError` from the body's `kind`, and the result is
returned to `main.rs`'s existing rendering path. The exit code, the `--json`
envelope on stderr, and the prose on a terminal are all unchanged from the
local case, because they are produced by the same code.

Transport failures — connect refused, a reset, a truncated read — become
`unavailable`, exit 7. That is deliberately neither `conflict` (exit 2, which
asserts a store looked at the request and refused it) nor `internal` (exit 1,
which the exit-code table calls unretryable and a bug). A caller must read
exit 7 as *unknown outcome*: in the window where the request was sent and the
response was lost, the transaction may have committed. The recovery is the
process contract's — read before writing.

## Plaintext

`check_url` reuses `s3::is_loopback_literal` rather than growing a second
opinion about when plaintext is acceptable. `https` to anything; `http` only to
`127.0.0.0/8`, `::1` or the literal `localhost`. Writing `http://` in `--server`
is itself the explicit request for plaintext — the operator typed the scheme —
but it cannot make a non-loopback host acceptable, and a name that merely
resolves to loopback does not count, because a DNS answer is
attacker-influenceable and is not a security property.

Terminating TLS in a reverse proxy that speaks plaintext to loopback is a
legitimate deployment, and it satisfies this rule exactly as written.

## Identity travels in the body

`--as` is a field on the args struct, so it is already in the request body and
nothing here has to put it there. It is never derived from the socket, the
source address or the OS user: a wave coordinator is one process acting on
behalf of many agent ids, and connection-derived identity would need one
connection per agent. It is also unauthenticated — anything that can reach the
port can assert any agent — which is why the server binds loopback by default.
