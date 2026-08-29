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

## Deadlines, and the thread that holds the socket

`WORKERS` threads call `recv_timeout` on the listener, and that timeout bounds
only the wait for a *new* request. It used to do nothing once a worker was
inside a handler, and the body was read there — so four clients that sent
headers with a `Content-Length` and then stalled held every worker forever. No
malice required: a closed laptop lid mid-`POST` produces it.

The fix is shaped by one fact. **A blocking socket read cannot be interrupted
by the thread performing it, and `tiny_http` exposes no seam to set a timeout
on an accepted socket** — `Request` hands out a `&mut dyn Read` and nothing
else, and `from_listener` still owns the `accept`. So the thread that applies
the deadline can never be the thread inside the read. Each request therefore
gets a socket thread of its own that owns the connection for its whole life —
body in, response out — while the worker keeps the store work and the clock:

- `READ_DEADLINE`, five seconds, from "a worker dequeued this request" to "the
  body is here".
- `WRITE_DEADLINE`, thirty seconds, from "the response bytes were handed over"
  to "they reached the socket". A client that stops *reading* holds a worker
  exactly as a stalled body does; the kernel send buffer autotunes to megabytes
  and hides that for every ordinary payload, which is why the served *read*
  verbs are the reason this one exists.

`WORKERS` keeps meaning what it meant — requests in flight against SQLite —
because a socket thread never opens a store. That is why the count stays at
four: the useful concurrency is bounded by the write lock, every request is one
short `BEGIN IMMEDIATE`, and more threads would buy contention rather than
throughput.

**A worker that gives up frees itself, not the socket.** The abandoned thread
stays blocked until the peer sends, closes, or TCP keepalive expires, so a
stalled connection costs one thread and one descriptor for as long as the
peer's own stack keeps it alive. There is no third option without owning the
socket; a bounded, visible leak replaces an unbounded outage.

A body that arrives after the deadline is **refused, not run**: the abandoned
thread answers for itself with `400` / `invalid_input` naming the deadline —
the same kind an unreadable body already produced, and the same true statement,
that the promised body was not delivered. Answering rather than dropping is
deliberate: `tiny_http` writes a bare 500 for a request that is dropped
unanswered, and a slow client meeting a 500 would read a bug in the server
where there was a bug in its own link.

One thing that looks like a hole and is not: `tiny_http` buffers any body it is
told is **1024 bytes or smaller** on its own connection thread, before the
request reaches a worker at all. A stall below that threshold costs a
connection thread, is invisible to both deadlines, and cannot be fixed without
forking the crate — and does not need to be, because it cannot wedge a worker.
Only a declared length above it reaches the deadline, which is what the test in
`tests/serve.rs` sends.

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
startup, shutdown, every 4xx, every 5xx, a dropped response, a request
abandoned at its read deadline (`read_timeout`), and listener faults, and successful requests only behind `--access-log`. Request bodies are
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

It also carries `project_keys`, a name-to-`project_key` map, added beside
`projects` rather than folded into it: `projects` is what `remote::probe_server`
reads to decide this is a quipu front door at all, so turning that list into
objects would make every existing consumer wrong at once. It costs nothing to
answer — the keys were read at startup with everything else — and it is what
`qp remote doctor` needs, because a name comparison passes against a front door
serving a different project of the same name.

## A name is not an identity

A project **name** is machine-local by construction: a registry maps a name to a
path *per machine*. So two hosts each serving their own `alpha` is the ordinary
case rather than an exotic one, and on one machine it cannot even be reproduced
— the registry refuses two projects of one name, which is exactly why this gap
survived every same-host test.

Before the key crossed the socket, pointing a client at the wrong host's front
door took work from a project the caller had never seen and reported exit 0. So
`State::store` takes the caller's asserted key alongside the name, and
`check_identity` is the served twin of `db::init`'s `project_key_mismatch`.

`project_key` and not `project_uuid`: the key is the value a set of stores
*share*, which is what "the same project on another machine" means. The uuid is
per-store, so comparing uuids would fail the correct case — one project, two
machines, two stores — every single time.

The comparison is a string compare against a value read once, from the same
connection that proved the store openable before the listener bound. An identity
fixed before the first accept cannot drift, so there is no per-request query and
nothing to invalidate.

A mismatch is `no_store` — exit 6 — and deliberately neither `not_found` nor
`conflict`. Exit 2 means "look again, it may change"; this never changes,
because the store set is fixed before the listener binds and no request can add
to it. That is the same thing exit 6 already says for a project this process
never opened. The local twin is a `conflict` because there it genuinely is a
state conflict — `qp init` refusing to re-home a store — while here nothing is
being changed by anyone.

A caller that asserts no key is not checked, and that is not a hole this side
can close: see `main::asserted_project_key` for which callers assert and why a
client with no store of its own cannot.

