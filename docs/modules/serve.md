# `serve`

A foreground HTTP front door for N stores that already exist. It is not a
source of truth, not a lock manager and not a gatekeeper: the CLI keeps opening
the same files directly, so a store fronted by this process is a store with one
more writer, not a store with one writer.

## The crate

`tiny_http`: synchronous, thread-per-`recv`, no runtime. It brings three crates
that were not already in the tree (`ascii`, `chunked_transfer`, `httpdate`) plus
`log`, which `rustls` already pulled in via `ureq`. Hand-rolling HTTP over
`TcpListener` was rejected — request parsing is a security surface, and the
smallest crate that does the job properly is the rule.

## Concurrency: a connection per request, and why that is not the design sentence

The wire contract says the server "holds one connection per store file it
serves". This does not, and the departure is deliberate. Three facts decide it:

1. **`BEGIN IMMEDIATE` is the guarantee, not the server.** Serialisation comes
   from SQLite and always has. It was measured that N in-process connections
   behave exactly as N processes under it, against a `busy_timeout=0` control.
   So per-request connections are exactly as correct as per-process ones, which
   is the arrangement every guarded transition was already written for.
2. **A `Mutex<Connection>` per project serialises reads behind writes.** That is
   the thing the WAL work went to trouble to avoid. One mutexed connection would
   put a `show` behind a `complete` that is waiting on a lock a local `qp` holds.
3. **A read cursor held across a response pins the checkpoint and grows the WAL
   without bound.** A per-request connection cannot outlive its response, so
   nothing can hold a cursor open by construction — the payload is a fully
   materialised `Outcome` before a single byte is written to the socket.

A small pool per project was the third candidate and buys nothing here: it
trades an unconditional open for checkout bookkeeping, and the open is one
`sqlite3_open` plus four pragmas plus the idempotent `ALTER TABLE` probes that
every one of the thousands of CLI invocations already pays on its own hot path.

What the contract's sentence was really protecting is §2 of the process
contract, and that is honoured exactly: every named project is resolved and
`db::open`ed — migrations and all — *before* the listener binds, and the store
set is fixed from that moment. A project that was not resolvable at startup is
not served, and no request can make it appear, which is what makes a runtime
`no_store` mean "this front door does not serve that project" rather than
"something might be broken".

## Startup failure is not runtime failure

Codes come from the existing table; none is added. A bad or busy bind address is
`invalid_input` (1) — the operator's input. A project with no registry entry, or
whose store file is absent, is `no_store` (6) — the same thing code 6 says to a
CLI. A store that exists and will not open or migrate is `internal` (1). Exit 7
is never produced here: `unavailable` is client-side only, and a server that
exited 7 would be claiming a front door could not be reached while being it.

Once accepting, a per-request failure is a *response*. The process exits at
runtime for exactly one reason that is not a signal: `recv` itself failed, which
means the listener became unusable. An operator watching restart counts must be
able to read a restart as "the process died", never as "one request was
malformed".

## `SIGPIPE`

`main` restores `SIG_DFL` so `qp list | head -1` dies like a Unix filter. For a
server that is a remote kill: any client that disconnects mid-response delivers
`SIGPIPE` and takes down a process serving everyone else. `ignore_sigpipe`
narrows it back to `SIG_IGN` for the life of the serving process, so a socket
write to a departed peer returns `EPIPE` and costs that one request.

`SIG_IGN` rather than `MSG_NOSIGNAL` on every write: the socket writes belong to
`tiny_http`, so there is no seam for a per-write flag short of forking the
crate, and a per-call-site fix is the exact failure mode the CLI's own
disposition change was chosen to avoid. The narrowing lives here and not in
`main`, so the CLI's documented and tested filter semantics are untouched — a
process that runs `serve` never goes back to being a CLI.

## Shutdown

`SIGTERM` and `SIGINT` set an `AtomicBool` and nothing else; a handler may only
do async-signal-safe work, and closing a SQLite connection from a signal context
can deadlock against the transaction it interrupted. Workers block in
`recv_timeout` and observe the flag, the listener is dropped, and the process
exits 0 — a stop that was asked for is not a failure, and a supervisor
configured to restart on failure must not restart on it.

Durability does not come from the clean exit. It comes from SQLite: a committed
transaction is in the WAL, an uncommitted one is rolled back on the next open,
and there is no in-memory queue anywhere in this process.

## Logging

One JSON object per line on stderr, `eprintln!` of a `serde_json` value, and no
logging framework. The level is a field in the object, not a global filter with
a subscriber registry — filtering is `grep`'s job, after the line is written.

The rule that decides what is logged is: **log what SQLite does not record.**
`insert_event` writes inside the same transaction as the mutation, so
`qp timeline` is a better request log than any text file — transactional,
queryable, and unable to disagree with the state it describes. So stderr carries
startup, shutdown, every 4xx, every 5xx, a dropped response, and listener
faults, and successful requests only behind `--access-log`. Request bodies are
never logged: ticket titles and decision text are the user's content and have no
business in an operational stream a supervisor ships elsewhere.

Rotation, retention and routing belong to whatever supervises the process. There
is no `--log-file`; `qp serve 2>> <path>` makes it visibly the operator's
problem rather than invisibly quipu's.

## Health

`GET /healthz` answers from process-local state only — version, bind address,
uptime, and the names of the projects opened at startup, all of which are in
memory because startup put them there. It opens no store (opening runs
migrations, and a probe that migrates changes the thing it measures), queries
no store, takes no write lock, and accepts no project parameter — a liveness
probe whose answer depends on user input makes "unknown project" look identical
to "process unhealthy".

It returns 200 or nothing. There is no unhealthy body, which is also forced by
the wire contract: a 503 from this process would prove a front door answered
while `unavailable` means none did.

`/healthz` sits outside the verb namespace on purpose, so a project named
`healthz` can never collide with it.
