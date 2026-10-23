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

## A 2xx is not by itself an answer

Every 2xx whose body was valid JSON used to be accepted, of any shape, and the
read views render a mismatch as *empty*. So a service answering `200 {}` on
every path produced an empty table from `list`, all zeros from `status`, and a
fabricated blank ticket from `show` — all at exit 0. `qp remote doctor` never
had the bug, because it checks the `/healthz` body is a quipu health payload;
this path checked nothing.

`call` now requires `wire::VERSION_HEADER` on a 2xx. The
refusal is `invalid_input` (exit 1) and not `unavailable` (exit 7): something
answered, so the front door was reached, and a caller that read this as "nothing
is listening" would send an operator to start a process on a port already held.
It is checked on 2xx only — a non-2xx from a foreign service is already loud,
because `wire::error_from` refuses a body that is not the error envelope.

`health` deliberately does not check it. A front door old enough to predate the
header would then be reported by `qp remote doctor` as "not a quipu front door"
rather than as the version skew it is, and telling those two apart is the whole
job of that command. The probe returns the body and the doctor judges it, which
it already does more thoroughly than a header check could.

## The path in a `--server` url is kept, and a subpath is supported

`check_url` preserves the path and `call` appends `/v1/<verb>`, so
`--server https://ops.example.com/qp` issues `POST /qp/v1/list`. That is the
ordinary reverse-proxy deployment and it is supported; what it requires is that
the proxy strips its own mount point before forwarding, because `qp serve`
routes on `/v1/<verb>` and has no notion of a base path. A prefix that reaches
the process unstripped is refused as `invalid_input` (exit 1) with the remedy
named — never as `not_found`, which is exit 2 and which the documentation tells
a skill to retry.

## The other identity: which PROJECT, not which agent

`--as` says who is asking. `project_key` says which project they think they are
asking about, and the two are unrelated concepts that happen to share the word.

It rides in the query string beside `project`, not in the body, because both are
**addressing**: `project` says which store, `project_key` says which project that
store had better be. Neither is an argument to the verb. A body field would also
have to be added to every served args struct — all of which set
`deny_unknown_fields` — for a value that is a property of the invocation rather
than of the command.

The client sends it only when it is naming its own project; `main::asserted_project_key`
holds the rule and the reasoning. A client with no store on disk sends nothing,
and the guard does not apply to it: a storeless worker has no recorded key to
be wrong about, so `--project alpha` *defines* alpha as whatever that front door
calls alpha.
