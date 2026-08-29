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

## `health` is the one request that is not a verb

`GET /healthz`, with no body and no project parameter — the process contract
fixes both. It exists so a diagnostic can ask a front door what it is and what
it serves without opening a store, and it returns the parsed body rather than a
verdict: judging that body is the doctor's job, and a client that collapsed
"nothing answered" into "answered something unexpected" would erase the one
distinction the diagnosis is for.

So the two failure shapes are kept apart deliberately. A transport failure and
a truncated read are `unavailable`, like every verb's. A 200 whose body is not
JSON is NOT: something answered, the front door was reached, and a caller that
read that as "nothing is listening" would send an operator to start a process
that is already running — on a port already held by something else.

Its body ceiling is much smaller than a verb's, because the probe is aimed at a
url that may not be a quipu front door at all and the first thing it can meet is
some other service's home page.

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
