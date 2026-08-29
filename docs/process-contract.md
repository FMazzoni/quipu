# The process contract

What `qp` guarantees to whatever supervises a long-lived `qp` process, and what
it requires back. This is the operational half of the wire contract: that
document settles what goes over the socket, this one settles how the process
that holds the socket is started, restarted, stopped and logged.

> **This document is written by an LLM about code it also wrote.** Same caveat
> as `architecture.md` and `wire-contract.md`: treat it as a map. Claims about
> current behaviour are checkable against the source and should be checked.

It is a contract for *any* operator. It names no host, no machine and no
deployment, because there is no single right place to run this — there is only
a set of properties the process must have wherever it runs.

## The premise: there is no retry anywhere

Everything below follows from one checked fact, so it is stated first.

**Nothing in `src/` retries a failed operation.** The tree was read for it. What
exists is three things that are not retries:

- `busy_timeout` in `db.rs` — SQLite blocking *inside a single call* while
  another writer holds the lock. One call, one outcome; nothing is re-issued.
- The 500 ms poll loops in `wait` and `watch` — repeated *reads* looking for a
  state that has not arrived yet. Re-reading is not retrying a write.
- Continuation-token paging in `s3.rs` — a sequence of distinct requests, not
  the same request twice. Its own header says the design has "no
  retry-on-rejection either", and explains why the object store does not need
  one.

So a request that fails in transit fails, once, and the exit code reaches the
caller. There is no layer that will quietly paper over a restart. Every
restart-window decision below has to be answerable by the *caller*, because
there is nobody else to answer it.

## 1. Foreground, never self-daemonising

**Decision: the process runs in the foreground, in the terminal or supervisor
that started it, until it is told to stop. It never forks, never calls
`setsid`, never writes a PID file, and takes no `--daemon` flag.**

This is the correct call and it should be made now rather than un-built later.
Every supervisor written in the last fifteen years — systemd, launchd, runit,
s6, supervisord, Docker, any container runtime — tracks the process it spawned.
A double-fork severs exactly that link: the supervisor's child exits
immediately, so "started successfully" becomes indistinguishable from "crashed
during startup", restart counting breaks, and the supervisor's log capture is
attached to a process that no longer exists. Recovering from that means a PID
file, and a PID file is a stale-state bug waiting for a hard reboot.

Concretely, for a reader who has to act on this: the process must hold the
supervisor's stdout and stderr for its whole life, and it must not return from
`main` until it is stopping. If a future flag makes it detach, every other
guarantee in this document stops being checkable.

**Rejected: a `--daemon` flag "for convenience".** The convenience is
`nohup qp serve &`, or a supervisor, and both already exist. Adding detaching to
the binary imports the whole PID-file problem to save one word at a prompt.

## 2. Startup failure is not runtime failure

**Decision: everything the process needs for the life of the run is acquired
before the listener accepts its first connection, and any failure to acquire it
is a non-zero exit before that point. Nothing is opened lazily on first
request.**

The failure this prevents is specific and nasty: a process that binds a port
successfully and *then* discovers it cannot open one of its stores looks
perfectly healthy to a supervisor — it is running, it is accepting, its health
probe passes — while answering `no_store` to every request for that project,
forever. Restarting it does not help, because from the supervisor's point of
view nothing is wrong. Fail-fast turns that into a failed unit with a message,
which is a thing an operator can see.

So the startup sequence is: parse arguments, resolve every named project
through the registry, open a connection to every resolved store (running
migrations, as `db::open` already does), bind the listener, *then* accept. Any
step failing means exit.

**The store set is fixed at startup.** A project that is not resolvable when
the process starts is not served by that process, and no request can make it
appear. That is what makes `no_store` at runtime mean "you asked for a project
this front door does not serve" rather than "something might be broken".

### Which exit code

From the existing table in `architecture.md`. **No new code is added**; codes
0–7 are taken and the table is a contract agents branch on.

| startup failure | kind | exit |
|---|---|---|
| unparseable or unusable bind address, port already in use | `invalid_input` | **1** |
| a named project has no registry entry, or its store file is absent | `no_store` | **6** |
| a store file exists but cannot be opened or migrated | `internal` | **1** |
| nothing failed | — | listener accepts |

The split follows the same question the wire contract uses to pick a status:
could a different invocation have worked? A busy port and a typo'd bind address
are the operator's input, so they are `invalid_input`. A missing store is the
machine not being attached to that project, which is exactly what code 6 was
introduced to say, and it says it identically whether a CLI or a server hit it.
A store that exists and will not open is a genuine fault, so it is `internal`.

**Exit 7 is never produced by this process.** `unavailable` is client-side only
— the wire contract is explicit — and a server that exits 7 would be claiming a
front door could not be reached while being the front door.

### Runtime failure is the opposite

Once accepting, a per-request failure is a response, not an exit. A malformed
body, a lost race, a missing task: all of these are the normal error envelope
over the socket, and the process keeps serving. **The process exits at runtime
for exactly one reason that is not a signal: the listener itself became
unusable.** A single request can never take the process down, and in
particular an `internal`/500 is a 500 and nothing more — an operator watching
restart counts must be able to read a restart as "the process died", not "one
request was malformed".

## 3. Shutdown

**Decision: `SIGTERM` means stop, and a handler is installed — but the reason
is bookkeeping, not durability. The default disposition is already correct for
everything that matters.**

*Amended when this was first implemented.* The original decision went further
and said quipu was allowed to ship with **no handler at all**. That sentence
contradicted two obligations stated within a few lines of it: the exit-0
requirement below, and the shutdown log line in §5. A process terminated by the
default disposition never runs another instruction, so it cannot log its own
exit, and its wait status is death-by-signal rather than exit 0 — which a
supervisor configured to restart on failure reads as a failure. The handler
exists to make those two true. Everything the original paragraph said about
*durability* stands unchanged and is the important half.

That durability argument is worth stating plainly, because the instinct is to
reach for a graceful-shutdown mechanism first and justify it afterwards. Durability here
does not come from a clean exit. It comes from SQLite: a transaction that
committed is on disk in the WAL, and a transaction that did not commit is
rolled back on the next open. Killing the process between two requests loses
nothing, and killing it *during* a request loses only that request, which had
not committed. There is no in-memory queue, no write-behind buffer, and no
state that exists anywhere but the store files.

So the obligation on shutdown is small and it should stay small:

- **Stop accepting.** New connections are refused rather than accepted and
  dropped, so a client sees a connect failure — which the wire contract already
  gives a meaning to — instead of an empty response it has to guess about.
- **Let an in-flight transaction finish or roll back.** They are short: one
  `with_tx` under `BEGIN IMMEDIATE`, no network inside the lock.
- **Close each store connection**, which lets SQLite's last-close checkpoint
  fold the WAL back into the main file. This is a courtesy to the next start,
  not a correctness requirement.
- **Exit 0.** A stop that was asked for is not a failure, and a supervisor
  configured to restart on failure must not restart on this.

`SIGINT` is treated identically, for the operator who started the process in a
terminal and pressed Ctrl-C. `SIGHUP` is **not** reload and **not** log-reopen
(see §6); if it is ever given a meaning, it will be a deliberate decision with
its own reasoning, and until then its default disposition is fine.

### How a signal is handled with no async runtime

There is no signal handling in the tree today and no crate for it, but there is
a precedent for how this project reasons about signals:
`restore_sigpipe_default` in `main.rs` is a single `libc::signal` call in a
documented `unsafe` block at startup, with a safety argument that turns on the
process being single-threaded and pre-output at that moment.

A stop handler takes the same shape and the same discipline. A signal handler
may only do async-signal-safe things, so it does exactly one: set a flag — an
`AtomicBool`, or a byte written to a self-pipe if the accept loop needs to be
woken from a blocking call. Everything above happens on the main thread, after
the accept loop observes the flag. **No cleanup runs inside the handler**: not
a database close, not an allocation, not a `println!`. A handler that closes a
SQLite connection from a signal context can deadlock against the very
transaction it interrupted.

### The `SIGPIPE` hazard, which is the real finding here

`main()` calls `restore_sigpipe_default()` unconditionally, restoring `SIGPIPE`
to `SIG_DFL` so `qp list | head -1` dies like a Unix filter instead of
panicking. That is right for a CLI and **actively dangerous for a server**:
with the default disposition restored, a write to a socket whose peer has gone
away delivers `SIGPIPE` and kills the whole process. Every client that
disconnects mid-response becomes a remote-triggered shutdown of a process
serving everyone else.

Whoever builds the server must handle this, and it is not optional: either
restore `SIG_IGN` for the lifetime of the serving loop so socket writes return
`EPIPE` as an ordinary error, or make every socket write use `MSG_NOSIGNAL`.
The CLI behaviour must not change — the filter semantics are documented and
tested — so the narrowing belongs in the serve path, not in `main`.

## 4. Restart semantics

**Committed state survives a restart. In-flight requests do not, and nothing
retries them.**

### What a client sees

Three windows, all of which resolve to the same kind:

| when the crash lands | what the client observes | kind | exit |
|---|---|---|---|
| before the connection is established | connect refused | `unavailable` | **7** |
| after the request was sent, before a response | connection reset, short read | `unavailable` | **7** |
| while the process is down | connect refused | `unavailable` | **7** |

This is precisely what `unavailable` was reserved for. It is deliberately not
`conflict` (exit 2) — no store looked at the request, so nothing refused it,
and a caller that reads a dead socket as a lost race will re-derive state that
never changed. It is deliberately not `internal` (exit 1), which the exit-code
table calls unretryable and a bug.

**The honest part: exit 7 does not tell you whether the write landed.** In the
middle window the transaction may have committed microseconds before the
process died, with the response lost on the way out. `unavailable`'s promise is
"the front door could not be reached", and in that window the front door was
reached. The kind is still right — it is the only one that is retryable without
asserting anything about store state — but a caller must treat exit 7 as
*unknown outcome*, not as *did not happen*.

### What an agent mid-wave should do

Concretely, in this order:

1. **Do not blindly re-issue a write.** The outcome is unknown, so a re-issue
   is a second attempt at something that may already have succeeded.
2. **Read before writing.** `qp show <id>` (or the served read behind it) is
   the answer to "did my write land". Every guarded transition is observable
   after the fact: the state moved or it did not, and `qp timeline <id>` is
   exact — `insert_event` runs inside the same transaction as the mutation, so
   the absence of an event is positive evidence that the transition never
   landed.
3. **Then re-issue only if the read says it did not land.** After the read, the
   re-issue is an ordinary request whose outcome is an ordinary exit 0 or 2.

Two things that are already true and that agents get wrong:

- **A claim committed before the crash is still held.** The claim is a row, not
  a server-side session. Nothing about it lived in the process that died, so
  the restart releases nothing and the agent that held it still holds it. An
  agent that assumes its claim evaporated and re-claims will get exit 2
  `already_claimed` naming *itself*.
- **The lease is still running.** Lease expiry is wall-clock against a
  timestamp in the store; it does not tick in the server. Downtime is not
  paused time, so a long outage can expire a lease that would otherwise have
  been renewed — and the recovery for that is the one that already exists,
  `qp reclaim` or lease expiry, exactly as for an agent that crashed. **A
  server restart is not a new failure mode; it is the existing crashed-agent
  failure mode arriving from a different direction.**

### Restart policy for the supervisor

Restart on failure, with a rate limit, not unconditionally and not infinitely.
The reasoning is §2: a startup misconfiguration exits non-zero, and a
supervisor that restarts it forever converts a legible error into a hot loop
that fills a log with the same line. A burst limit turns it into a failed unit
that an operator can see. A clean `SIGTERM` exit 0 must not restart at all.

### There is no offline mode, and reads fail closed too

**Decision: with a front door configured and unreachable, every served verb
fails — reads exactly like writes — with the `unavailable` kind and exit 7.
Nothing is served from a cache, nothing is queued for replay, and the store on
the client's disk is not a cache and must never become one.**

This is checked behaviour rather than a proposal. A client pointed at a dead
listener answers identically for `list`, `show`, `take` and `claim`:

```text
$ QP_SERVER=http://127.0.0.1:8801 qp list --json
{"error":{"endpoint":"http://127.0.0.1:8801/v1/list","kind":"unavailable",
          "message":"... : io: Connection refused"}}
$ echo $?
7
```

The read half is the half that gets argued about, so the argument is recorded
here rather than left to be re-had.

**The trap, stated first, because it is the reason for the whole position.**
Queuing writes for later replay rebuilds every mechanism the server design
exists to delete: two writers, divergence, conflict detection, and an ordering
rule for merges. `BEGIN IMMEDIATE` on one file is the arbitration, and a queue
is a second writer that has not been told so yet. Whatever else is decided,
it must not be that. A read cache is the same failure arriving politely: the
moment a stale row is good enough to answer from, it is a second source of
truth, and the next question is which one wins.

**A stale board is worse than a loud failure, specifically for an
orchestrator.** The consumer of a read here is not a human skimming; it is a
coordinator deciding what to dispatch. A cached `ready` cohort answers the one
question a dispatcher asks — *what is free* — with a snapshot of what was free,
and the dispatcher acts on it by assigning work that another machine already
holds. The refusal is recoverable in one line; the double dispatch is a wave
whose event log is honest about what happened and useless for working out why.
An error a caller cannot ignore beats an answer a caller cannot date.

**The local store is not the cache, and this is where divergence would sneak
back in.** It is the obvious candidate — a real SQLite file, the same schema,
sitting right there — and it is the worst one, because reading it and writing
it are one flag apart. There is no `--offline`, no fallback tier under the
recorded url, and `resolve_server`'s local tier fires only when *no* front door
was named at all. A machine with both a store and a front door is a machine
whose commands go over the wire; the file beside it is not consulted, not
compared, and not refreshed.

**What an agent mid-wave does is already written down and does not change.**
Exit 7 is *unknown outcome*, not *did not happen*: read before writing, and the
timeline is exact evidence because `insert_event` runs inside the mutation's
transaction. §4 has the sequence. A server that is down does not orphan a
claim — the claim is a row, not a session — and the lease keeps running on
wall-clock, so a long outage is the existing crashed-agent case and `qp reclaim`
is the existing recovery.

**The one path out is not a cache and is deliberately unserved.**
`qp remote clear --front-door` detaches the store, after which its commands read
the file beside them again. It works while the front door is down precisely
because `remote` is local-only: resolution never consults a recorded url for a
verb the front door does not answer. That is a deliberate act with an event
behind it, which is the difference between choosing to work from a local store
and being silently handed one.

**What this rules out, so a later slice does not read the silence as room.** No
response cache in `client.rs`. No write-ahead queue anywhere. No `--offline`,
`--stale-ok` or `--cache-ttl` flag. No fallback from a configured front door to
the local store on transport failure. Each of these looks like an availability
improvement and each converts a single source of truth into two.

## 5. Logging

**Decision: one JSON object per line, on stderr, for events that never reached
a transaction. No per-request access log by default.**

### What gets logged, and the reason the list is short

The store is already a better request log than any text file. `insert_event`
writes inside the same transaction as the mutation, so `qp timeline` is
transactional, can never disagree with the state it describes, and is queryable
— and, per the symptom index in `architecture.md`, the *absence* of an event is
positive evidence a transition did not land. A stderr line per successful
request is a second, weaker copy of that record, written outside the
transaction, guaranteed to drift, and — at wave rates, with a poll loop per
waiting agent hitting a read endpoint every 500 ms — the fastest way to fill a
disk that this project has yet invented.

So the rule that decides what goes on stderr is: **log what SQLite does not
record.** That is exactly the set of things that never became a transaction, or
that happened to the process rather than to a store:

- **Startup**: one line naming the version, the bind address, and each project
  resolved with the path it resolved to. This is the line an operator reads
  first when something is wrong, and §2 guarantees it is complete.
- **Shutdown**: one line naming the signal and the exit code.
- **Every 4xx**: the request was refused before or by the store; if it was
  refused *before* the store, this line is the only record anywhere.
- **Every 5xx / `internal`**: always, with as much detail as is available. A
  500 is a bug by definition, per the exit-code table.
- **Anything about the listener itself**: a failed accept, a socket-level
  error.

Successful requests are logged only behind an explicit opt-in flag
(`--access-log` or similar), off by default, for the operator debugging a
latency problem who has decided to pay for it.

### The format

One JSON object per line, `serde_json::to_string` of a small struct, emitted
with `eprintln!`. `serde_json` is already a dependency; a log line needs
nothing beyond it, and in particular needs no logging framework.

Fields: `ts` (RFC 3339, from `time::now_rfc3339` — never a second clock),
`level`, `event`, and then event-specific fields. For a request: verb, project,
HTTP status, error `kind` when there is one, and duration. **Not the request
body.** Ticket titles, decision text and log messages are the user's content
and have no business being duplicated into an operational stream that a
supervisor will ship somewhere else. The asserted `agent` is fine — it is free
text the caller chose and it is already in the store.

One format, always, with no human-readable variant and no terminal detection.
This is the same reasoning the wire contract uses for `human()` ownership: two
renderings of one thing is two things to test and two things to drift, and a
server's output is read by machines.

Levels are a **field in the object**, not a global filter with a registry and a
subscriber. Filtering is `grep`, or the log system's, and it happens after the
line is written.

## 6. Rotation belongs to the supervisor

**Confirmed. quipu writes to stderr and stops there. The process takes no
`--log-file` flag.**

The moment quipu opens a file it has inherited the entire problem, and none of
it is a problem about task orchestration:

- **Reopening.** A rotator that renames the file leaves the process writing to
  an unlinked inode, so every line after the rotation goes nowhere. Fixing that
  means a `SIGHUP` reopen handler — more signal handling, in a project that
  deliberately has almost none.
- **Disk full.** A write failure must not turn a serving process into a
  crashing one, so every log write needs a failure policy, and "drop the line"
  and "stop serving" are both wrong in some deployment.
- **Retention, permissions, ownership, compression, and ordering across
  restarts** — each with a correct answer that differs per operator.

Every one of these is already solved, once, by the thing supervising the
process: journald, logrotate, svlogd, a container runtime's log driver. Writing
a second implementation inside a static binary whose stated budget is zero
external services and a 30 ms cold start is the wrong trade in every direction.

The honest framing for an operator who wants a file: `qp serve 2>> <path>`, and
the rotation is then their shell's problem, visibly, rather than quipu's
problem, invisibly.

## 7. Health

**Decision: a dedicated `GET /healthz`, answering from process-local state
only. It touches no store.**

A supervisor needs to distinguish *running* from *serving*, and "the process
exists" does not do it — §2 exists precisely because a process can be up and
useless.

### What it must not do

- **It must not open a store.** Opening runs migrations; a probe that migrates
  is a probe that changes the thing it measures.
- **It must not query a store.** A probe on a ten-second timer that runs a
  query per store is a recurring read against a file a concurrent local `qp`
  wants to write. Under WAL a reader does not block a writer, but it is not
  free, and it makes the health check's cost scale with the number of projects
  and the size of the task table.
- **It must not take the write lock**, ever, for any reason.
- **It must not accept a project parameter.** A per-project probe is a
  liveness check whose answer depends on user input, and "unknown project" then
  looks identical to "process unhealthy".

### What it does

Answers from state captured at startup: the version, uptime, the bind address,
the names of the projects this process opened, and — as `project_keys` — the
`project_key` of each. All of that is in memory because §2 put it there.

*`project_keys` was added after the first verbs shipped.* A project name is
machine-local, so two hosts can each serve an `alpha` and they are different
projects; a payload carrying only names cannot tell an operator, or
`qp remote doctor`, which of the two answered. It is additive beside `projects`
rather than a rewrite of it, because `remote::probe_server` reads that list to
decide the answering process is a quipu front door at all. It changes nothing
about what this endpoint may do: the keys were read at startup, so no store is
opened, queried or locked to answer.

**It returns 200 or nothing.** There is no unhealthy response body. If the
process cannot answer, the connection fails and *that* is the signal — which
is also forced by the wire contract: 503 is reserved, and a quipu process must
never send one, because a 503 is proof a front door answered while
`unavailable` means none did. A health endpoint with a failure status would be
the first violation of that rule.

### Why not reuse an existing read

`status` and `list` were the candidates and both are worse. They run aggregate
queries over the task table, which is the cost §7 is trying to avoid; they take
a project, which drags in the previous objection; and `no_store` for an unknown
project is a 404 that a naive probe reads as a health failure. A liveness probe
should have exactly one thing that can make it fail, and that thing should be
the process being dead.

`/healthz` also sits deliberately outside the verb namespace, so a project
named `healthz` can never collide with it.

A deeper check — "can this process actually read store X right now" — is a
real operator need and a different tool. It is an explicit, manually-run
command, not something a supervisor executes on a timer.

## 8. Request deadlines, because a client can wedge a server without malice

**Decision: a worker waits at most five seconds for a request body and at most
thirty seconds for its response to reach the socket. Past either, it abandons
the connection and goes back to serving. The worker count stays at four.**

The failure this prevents needs no attacker. Four clients that open a
connection, send headers with a `Content-Length`, and then stop — a closed
laptop lid mid-`POST`, a peer that died without sending a RST, a link that went
away between the headers and the body — used to occupy all four workers
indefinitely. The listener's own `recv_timeout` bounds only the wait for a
*new* request; it does nothing once a worker is inside a handler. In the
topology this contract is written for, several machines polling over a long
run, the probability of at least one half-open connection is not small.

### The deadline cannot be enforced by the thread that is inside the read

This is the fact the design follows from: a blocking socket read cannot be
interrupted by the thread performing it, and the HTTP crate exposes no seam to
set a timeout on an accepted socket. So the thread that gives up can never be
the thread that is stuck, and the connection has to move to a thread of its own
— body in, response out — while the worker keeps the store work and the clock.

**The consequence to be explicit about: a worker that gives up frees itself,
not the socket.** The abandoned thread stays blocked until the peer sends,
closes, or the kernel's TCP keepalive gives up on it, which by default is
hours. A stalled connection therefore costs one thread and one file descriptor
for as long as the peer's own stack keeps it alive. That is a bounded, visible
resource leak in place of an unbounded outage, and there is no third option
without owning the socket.

### The values, and what they are defended against

- **Five seconds for the body.** It is two numbers at once — the longest a
  legitimate slow client may take, and the longest a stalled one may cost a
  worker — and the second is what keeps it small. A body is a handful of short
  strings written in the same call as the headers, so it is already in flight
  when a worker first sees the request. Five seconds is `wait`'s entire 500 ms
  poll period ten times over, and carries the 1 MB body ceiling at 200 KB/s: a
  client cannot be cut unless it is orders of magnitude slower at delivering a
  few hundred bytes than it is at waiting between whole requests. Against that,
  four simultaneously stalled clients cost one five-second hiccup that heals
  itself, once per stall event, rather than a front door that never answers
  again.
- **Thirty seconds for the response.** A client that stops *reading* holds a
  worker exactly as a stalled body does. It is bounded more loosely because the
  legitimate case is larger and the failure rarer: the kernel send buffer
  autotunes to megabytes and swallows every ordinary payload without blocking
  at all, so a write still going after thirty seconds means the peer stopped
  reading. The served *read* verbs are why this exists at all — a write verb's
  payload could never reach the buffer's ceiling.

### What a cut client sees

A body that arrives after the deadline is **refused, never run**. The abandoned
thread answers for itself the moment the read completes: `400` with
`kind: "invalid_input"` and a message naming the deadline. That is the same
kind an unreadable body already produced, and it says the true thing — the body
the `Content-Length` promised was not delivered in time. It is deliberately not
`unavailable`: this contract's exit 7 means no front door answered, and one did.

A client that never sends at all sees nothing, because there is nothing to
write to. Its own transport failure is `unavailable`/7 by the ordinary route.

A response cut by the write deadline is the middle restart window of §4 with a
different cause: the transaction committed and the caller did not hear it. The
guidance there is unchanged — read the store, do not assume.

### The worker count stays at four

It was re-examined rather than left unexamined next to a bug it made worse, and
nothing was changed. The useful concurrency is bounded by SQLite's write lock,
not by the thread count: every request is one short `BEGIN IMMEDIATE`, so more
workers buy contention on the same lock rather than throughput. And the fix
removes the coupling that made four look too small — a socket thread never
opens a store, so four still means "requests in flight against SQLite" exactly
as it did before there was a deadline.

### Two new log lines

Both belong to the "log what SQLite does not record" rule of §5, because
neither request became a transaction:

- `read_timeout` (`warn`) — method, url, elapsed ms, the deadline. The only
  record anywhere that the request existed.
- The ordinary `request` line gains a `write_error` reading
  `response not written within 30s` when the write deadline expires.

### A stall smaller than 1 KB never reaches a worker at all

Recorded because it looks like a hole in the above and is not. The HTTP crate
buffers any body it is told is 1024 bytes or smaller on its own connection
thread, before the request is handed to a worker. Such a stall costs a
connection thread and is invisible to every deadline here — it cannot be fixed
without forking the crate, and it does not need to be: it cannot wedge a
worker, which is the property this section is about. Only a declared length
above that threshold reaches the deadline, and that is what the test uses.

## An example supervision unit

Generic and illustrative. Every path, user, port and project name below is a
placeholder; substitute your own. Nothing here is a recommendation about where
to run this.

```ini
[Unit]
Description=quipu task substrate server
After=network.target

[Service]
Type=exec
User=quipu
Group=quipu
ExecStart=/usr/local/bin/qp serve --bind 127.0.0.1:8787 --project PROJECT_NAME
WorkingDirectory=/var/lib/quipu

# Foreground process, stderr to the journal (see the logging section).
StandardOutput=journal
StandardError=journal

# Restart on failure only, rate-limited: a startup misconfiguration should
# land in a failed unit, not a hot loop.
Restart=on-failure
RestartSec=5s
StartLimitIntervalSec=60
StartLimitBurst=5

# SIGTERM is stop; give in-flight transactions a moment to finish.
KillSignal=SIGTERM
TimeoutStopSec=15

# The process needs its store files and nothing else.
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/quipu

[Install]
WantedBy=multi-user.target
```

Four lines carry the decisions above and are the ones to keep if you rewrite
this for a different supervisor. `Type=exec` requires §1's foreground process
and nothing else will work. `Restart=on-failure` with a burst limit is §4.
Journal capture of stderr is §5 and §6 together — nothing writes a file.
`KillSignal=SIGTERM` with a stop timeout is §3, and the timeout only has to
cover one short transaction.

Binding to `127.0.0.1` is not decoration: the wire contract's identity section
says `--as` is unvalidated free text, so anything that can reach the port can
act as any agent. A non-loopback bind is a deliberate act that needs
authentication in front of it, and there is none yet. The next section is that
sentence worked all the way through, because "how does a second machine reach
this" is the question the unit above otherwise leaves an operator to answer by
guessing.

## 8. The reach path

The reason this process exists at all is several machines, each running agents,
working different tickets in one project. That requires a second machine to
reach the front door. **Exactly one arrangement makes it possible, nothing in
the flags hints at it, and an operator meets it first as a refusal** — which is
the failure this section is here to fix.

**Decision: `qp` never carries a request over a plaintext link that leaves the
machine. The listener stays on loopback and encryption is terminated by
something in front of it. The supported arrangement is a TLS-terminating
reverse proxy on the serving host, with remote clients pointed at an `https://`
url.**

Nothing new is built for this; the arrangement falls out of three facts that are
already in the tree and are worth checking rather than taking on trust:

- **The bind default is loopback.** `ServeArgs::bind` in `cmd/serve.rs` defaults
  to `127.0.0.1:8787`, and its own doc comment gives the reason: `--as` is
  unvalidated free text.
- **The client refuses plaintext off loopback.** `client::check_url` rejects
  `http://` to any host that is not a loopback literal, delegating to
  `s3::is_loopback_literal` so there is one opinion in this codebase about when
  plaintext is acceptable rather than two. The recorded front-door url goes
  through the same gate — `remote::validate_server_url` calls `check_url` — so a
  url that stores clean is a url that will be called.
- **A name that merely resolves to loopback does not count.**
  `is_loopback_literal` accepts `127.0.0.0/8`, `::1` and the literal `localhost`
  and nothing else. A DNS answer is attacker-influenceable, so "points at
  127.0.0.1 right now" is not a security property.

Compose them and there is one shape left: the remote client's url is `https://`,
so something in front of the process must answer TLS, and that something speaks
plaintext to `127.0.0.1:8787`. The `qp serve` unit above does not change — the
proxy is the part that holds the certificate and the external interface.

**There is no override.** No flag and no environment variable turns the client's
refusal off; `check_url` has no bypass, and the harness variable that lets `s3`
talk plaintext to a loopback test endpoint cannot reach it. That is deliberate:
the refusal is the only thing standing between an unauthenticated write endpoint
and a network, so it must not have a switch.

**An SSH forward is the same arrangement, not an exception to it.**
`ssh -L 8787:127.0.0.1:8787 <serving-host>` leaves the client typing
`http://127.0.0.1:8787`, which is a genuine loopback literal on the client's own
machine, and leaves the bytes on the wire encrypted by ssh. The invariant holds
exactly as written — plaintext only ever crosses loopback — and the
authentication is ssh's. It is the ad-hoc answer for one operator at a keyboard;
the reverse proxy is the answer for a deployment, because it is the one a
supervisor can start and a fleet of agents can share.

### Authentication: read this before you expose the port

**The server authenticates nobody, and this is the moment to decide whether that
is acceptable, rather than after the port is open.**

The position, stated deliberately rather than left as an omission:

- **The process has no notion of a caller.** No credential is read, no header is
  inspected, no peer is identified. `cmd/serve.rs` reads exactly one thing off a
  request that is not the body: the url. There is no token to configure and none
  is planned in this document.
- **Whatever fronts it is the authentication boundary.** The reverse proxy, the
  ssh daemon, the network the port is on — one of those is deciding who gets to
  the socket, and if none of them is, nothing is.
- **A caller that reaches the port is trusted completely.** It may name any
  agent on any ticket: claim as someone else, complete someone else's work,
  reclaim a live lease. `--as` is a string the caller chose.

**The line, and it is a sharp one.** This is acceptable exactly as long as
everyone who can reach the front door is someone you would hand a writable copy
of the store file to. The server grants no authority the file does not already
grant — that is the honest version of "no worse than the file" — but it grants
it *at a distance*, to whoever the network lets through, and the set of people
who can open a TCP connection is not the set of people who can open a file on
your disk. A single owner running agents on machines they own is on the safe
side of that line. A shared host, a team, a network with a guest, a proxy that
takes no credential: all on the other side, and crossing it is not a
configuration change but a decision that this project has not built the
mechanism for. **If you cannot name every principal that can reach the port,
do not open it.**

The consequence for the proxy is one sentence, and it is the one operators skip:
**a TLS-terminating proxy that requires no credential has not added
authentication.** It has added encryption, which stops the network reading
traffic in flight, and it has satisfied `check_url`, which was never a claim
about who the caller is. Terminating TLS and forwarding everything is a
correctly-encrypted anonymous write endpoint.

### The arrangement that must not ship

`qp serve --bind 0.0.0.0:8787`, with remote clients on `http://<host>:8787`.

Say it plainly because it is the *first* thing an operator reaches for, and they
reach for it at exactly the wrong moment: the client has just refused their url,
the refusal mentions the host, and changing the bind looks like the fix. It is
not the fix and it is not even a fix — the refusal is client-side and keyed on
the host in the url, so a wider bind does not make `http://<host>` acceptable.
It only removes the second of the two guards while leaving the first one
refusing.

What that bind actually publishes is an unauthenticated write endpoint for every
served verb, in the clear, on every interface. Every ticket in every project the
process opened can be read, claimed, edited and completed by anything that can
route a packet to it, under any agent name it likes, and the store's event log
will faithfully record the agent name the attacker chose. There is no
credential to guess, because there is no credential.

So: if a client refused your url, the answer is a terminator in front of the
process, never a wider bind. The bind stays on `127.0.0.1`.

### Amendment: publishing a container port

*The section above was written for a process on a host and, taken literally,
forbids the only bind that works inside a container. It is amended here rather
than edited, because the sentence it retracts is the one an operator will quote
back.*

**What is retracted is narrow: "the bind stays on `127.0.0.1`" is a statement
about a *host's* loopback, and inside a container it is not the same interface.**
`docker run -p` forwards to the container's own interface address, not to its
loopback, so a `qp serve` bound to `127.0.0.1` inside a container is reachable
from nothing at all — including from the published port that appears, correctly
configured, in `docker ps`. Read as written, the section demands an arrangement
that cannot answer a request.

**What survives is the invariant, which was never about the literal
`127.0.0.1`: plaintext must cross only a loopback interface.** In a container
the network namespace *is* the boundary the host's loopback was standing in
for. Nothing but the runtime can reach a container's interfaces unless a port is
published, so `--bind 0.0.0.0:8787` inside a namespace publishes to that
namespace and no further. The invariant is then carried entirely by the publish
form:

```text
docker run -d --name qp -p 127.0.0.1:8787:8787 -v qp-store:/var/lib/quipu quipu
```

`-p 127.0.0.1:8787:8787` binds the *host* side to the host's loopback. A packet
reaching the process has crossed the host's loopback and one namespace hop, and
has never been on a network. Everything §7 and the client's `check_url` require
is satisfied unchanged: a remote client still types `https://`, still reaches a
TLS terminator, and that terminator still speaks plaintext to `127.0.0.1:8787`
— which is now a published container port rather than a process, a difference
the terminator cannot see and does not need to.

**What stays forbidden, and it is the whole of what the section above was
aimed at:**

- **`-p 8787:8787`, or `-p 0.0.0.0:8787:8787`.** The short form publishes on
  every host interface. This is the forbidden arrangement exactly, arriving
  through a flag that looks like it is about the container. It is the default
  a reader will copy, so it is named here.
- **`--network host` with any bind but loopback.** With no namespace there is
  no boundary left, and `0.0.0.0` means what it always meant.
- **`qp serve --bind 0.0.0.0` on a host, outside a container.** Unchanged, and
  still the first thing an operator reaches for at the wrong moment.

The test to apply is one question, and it is the one the original section was
asking: **can anything reach this port without first crossing an interface you
control?** A namespace answers no. A wider host bind answers yes, and no
container has changed that.

**Authentication is not amended.** Publishing to host loopback keeps the
plaintext invariant; it grants no credential and identifies no caller. Every
sentence in "Authentication: read this before you expose the port" applies to a
container unchanged, and the set of principals that can reach a loopback-published
port is every local user on that host.

### The deployment smoke, and the one thing it must assert

`Dockerfile` at the repo root is the generic image: a builder stage, the binary,
a store directory, and a foreground entrypoint. `docker-smoke.sh` is the
procedure that proves it deploys, and `just docker-smoke` runs it. It names no
host and no real volume; the port is an environment variable with a placeholder
default.

What it checks, in order: the container starts and `/healthz` answers with the
version and the project list §7 promised; a client **outside** the container
drives `add`, `list`, `take` and `complete` through the published port, from a
directory with no store in it; `docker restart` preserves committed state while
a client in the gap gets `unavailable` and exit 7, per §4; and a named project
whose store is missing **refuses to start**, per §2, rather than coming up
healthy and answering `no_store` forever.

**The assertion that is the point of the whole exercise is the store's
filesystem.** SQLite's locking is correct on local disk and is not correct over
NFS or SMB, so a deployment that bind-mounts stores from a share passes every
one of the checks above and corrupts under contention. The smoke therefore
asserts, rather than assumes: the volume's driver is `local`, it carries no
driver options (a `local` volume can still be an NFS mount through them), and
the filesystem type observed at the mount point inside a container is not a
network filesystem. A smoke without that assertion is worse than no smoke,
because it manufactures confidence in the one arrangement that must not ship.

What the smoke does **not** cover, said plainly so nobody reads a green run as
covering it: a TLS terminator, a real network hop, and any host but the one it
ran on. Those stay the manual residue — point a client at the running front
door across the network once, and run `qp remote doctor`.

### The forward-compatible shape survived implementation

The wire contract separated *asserted* identity from an *authenticated
principal* and required that they never share a field, because that separation
is the whole of what makes adding authentication additive later. It survived,
and it is worth recording as checked rather than assumed:

- **Asserted identity is a body field and only a body field.** `--as` is
  `#[arg(long = "as")] pub agent: String` on the args struct, serialised into
  the request body by `route::fetch` along with the rest of the arguments.
- **The server never infers it.** `cmd/serve.rs` reads no request header at all
  — the only `Header` in the file is the `Content-Type` it writes on the way
  out — and consults no OS user, environment variable or peer address. There is
  no code path by which `agent` arrives from anywhere but the explicit field,
  which is precisely the rule the wire contract said a later slice would be
  tempted to break.
- **The header namespace is untouched and free.** Nothing reads one, so an
  `Authorization` header can be added without changing the meaning of any
  existing field, and an unauthenticated build simply has no principal to check.

Adding authentication is therefore still the additive change it was designed to
be: a credential on the request, a principal derived from it, and a policy
relating that principal to the `agent` values it may assert. The body stays
exactly as it is.

## What this contract does not settle

- **The `serve` verb itself** — its flags, its listener, its HTTP layer. This
  document constrains that code; it does not write it.
- **An authentication mechanism.** §8 settles the *position* — the process
  authenticates nobody and whatever fronts it is the boundary — and the wire
  contract's identity section settles the *shape* a credential would take when
  one is built. Neither builds one, and this document does not propose one.
- **How to configure a TLS terminator.** §8 fixes that there must be one and
  what it must not omit; which proxy, which certificate authority and which
  renewal mechanism are the operator's, exactly like the choice of supervisor.
- **Backups.** The store is a SQLite file and the existing `export` / `sync`
  path is the answer; nothing about running a server changes it.
