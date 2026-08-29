# The wire contract

What `qp` sends and receives when a store is fronted by a server, and why each
shape is the one it is. This document is a *contract*: three later slices — a
vertical slice through add/assign/claim/complete, a fan-out of the remaining
write verbs, and a read side — are built against it and should not have to
reopen any decision here. Each section states the decision, the reasoning, and
what was rejected, so that reopening one is an argument rather than a guess.

> **This document is written by an LLM about code it also wrote.** Same caveat
> as `architecture.md`: treat it as a map. Claims about current behaviour are
> checkable against the source and should be checked.

No server, client, or HTTP code exists yet. The only code this document ships
is the `unavailable` error kind, reserved so a later slice cannot renumber it.

## The server is not the writer

Read this before anything else, because a reader who gets it wrong will design
the wrong system.

The serialisation guarantee comes from `BEGIN IMMEDIATE` inside SQLite, and it
always has. It does **not** come from the server being the only writer, because
the server is not the only writer: the CLI keeps opening the same files
directly, dozens of call sites call `db::open` with no layer in between, and
nothing in this design removes them. A store fronted by a server is a store
with one more writer, not a store with one writer.

So the server is an HTTP front door for N existing stores. It holds one
connection per store file it serves and `--project` selects among them, but it
is not a source of truth, not a lock manager, and not a gatekeeper. Every
guarded transition stays a single conditional `UPDATE ... WHERE state IN (...)`
plus a `changes() == 1` check, exactly as it is today, and it stays that way
*because* a concurrent local `qp` may be doing the same thing at the same
instant. A design that relaxes a guard on the grounds that "the server
serialises writes" is wrong, and will corrupt state the first time someone runs
`qp claim` in a terminal.

The corollary that matters for the envelope: the server adds no consistency
that the file did not already have, so the wire contract's whole job is to
carry the *same* answers the CLI already produces, unchanged, plus the one
thing the local caller got for free and the remote caller does not — knowing
what else moved (see "Collateral state changes").

## 1. The envelope

**Decision: the wire keeps the bare shape. The discriminator is the HTTP
status.**

A successful response body is exactly the bytes `serde_json::to_string(&o)`
produces today for the command's `Outcome` — a bare JSON object, no wrapper:

```json
{"display_id":"QP-7","agent_id":"w-alpha","state":"running"}
```

A failed response body is exactly the `{"error": {...}}` envelope `main.rs`
already writes to stderr:

```json
{"error":{"kind":"conflict","code":"already_claimed","message":"QP-7 already claimed","task":"QP-7"}}
```

Success and failure are told apart by the status: `2xx` carries the first
shape, everything else carries the second. That is the same arrangement the CLI
already has, with one substitution. Locally, success is disjoint from failure
twice over — by stream (stdout vs stderr) and by exit code. HTTP has one body,
so the stream half disappears; the status takes the exit code's place, and one
discriminator is enough because it was enough before.

**Rejected: an `{"ok":true,"data":{...}}` wrapper.** It fails whichever way it
is applied. Apply it only on the wire and the two shapes genuinely diverge:
every client must unwrap before printing, and there are now two JSON grammars
for one system. Apply it to the CLI as well and it is a breaking change to
every existing `--json` consumer, buying nothing — a wrapper earns its keep
only when success and failure share a channel with *no* other discriminator,
and HTTP's status line is exactly that discriminator, mandatory and impossible
to omit.

### Where the translation happens

There is one JSON grammar and two places that attach the out-of-band
discriminator to it. Both are single points, which is what stops this from
being two contracts:

- **Server side.** The error-rendering block in `main.rs` — the one that
  downcasts to `QuipuError`, calls `to_json()`, and calls `exit_code()` —
  generalises into a function `anyhow::Error -> (StatusCode, serde_json::Value)`.
  It uses the same `to_json()` and the same taxonomy; only the second element
  of the pair is new. Nothing in any `cmd::*::run` changes.
- **Client side.** The inverse: `(StatusCode, body) -> anyhow::Error`,
  reconstituting a `QuipuError` from the body's `kind`, and then handing it to
  the *existing* `main.rs` rendering path. The client prints and exits exactly
  as it does locally.

That inverse is the property to protect above all others: **an existing skill's
exit-code branch keeps working with a server in the path.** A conflict is exit
2 whether the store was local or remote, and a wave orchestrator that retries
on 2 needs no change at all.

## 2. The error taxonomy on the wire

### The new kind

`unavailable`, exit code **7**. A transport failure: the front door could not
be reached, so no store saw the request.

It must not be `internal` (exit 1), which `architecture.md` documents as
unretryable and a bug. It must not be `conflict` and friends (exit 2), which
assert that a store looked at the request and refused it — a store that was
never reached refused nothing, and a caller that reads a dead socket as a lost
race will re-derive state that never changed. It is retryable like 2 but
asserts nothing about the store like 1, so it is neither, and it takes its own
code. Seven, not six: six was taken by `no_store` while this contract was being
written, and "the next free code" is not a decision procedure when two people
are choosing at once.

Envelope body: `{"kind":"unavailable","message":"...","endpoint":"..."}`.
`endpoint` is the URL that failed when there is one to name, and it is its own
field rather than prose inside `message` so a caller can branch on it.

Nothing in the CLI constructs it today. That is correct and expected: it is
reserved for the client half, and pinned by `kind_and_exit_code_are_a_contract`
in `db.rs` so a later slice cannot quietly renumber it.

### Status for every kind

| `kind` | exit | HTTP | who produces it |
|---|---|---|---|
| *(success)* | 0 | **200** | server |
| `invalid_input` | 1 | **400** | server |
| `not_found` | 2 | **404** | server |
| `no_store` | 6 | **404** | server |
| `conflict` | 2 | **409** | server |
| `not_owner` | 2 | **409** | server |
| `invariant` | 2 | **422** | server |
| `internal` | 1 | **500** | server |
| `unavailable` | 7 | *(none)* | client only |

Two rows need their reasoning stated, because both look wrong at a glance.

`not_owner` is **409, not 403**. A 403 promises an authorization system, and
there is none (see "Wire identity"). `not_owner` is a statement about store
state — a different agent holds the open assignment — and it belongs with
`conflict`, which is the same kind of statement. When authentication does
arrive, a request rejected *before* reaching the store will be a genuine 401 or
403, and it must be distinguishable from this.

`invariant` is **422, not 409**, because the two answer different questions.
409 means "not now" — the same request may succeed once state moves. 422 means
"not ever, against this graph" — a dependency cycle does not resolve by
waiting. Both are exit 2 today, which is a coarser distinction than the wire
can make, and that is fine: the wire may be finer than the exit code, never
coarser.

`no_store` and `not_found` share 404 and are told apart by `kind`, which is the
point of the next rule.

### HTTP status is not the taxonomy

**`kind` is the contract; the status is a lossy projection of it for proxies,
`curl`, and load balancers.** A client MUST branch on `kind` and MUST NOT map a
status back to a kind — the mapping is not injective and was never meant to be.
This is not a new rule; it is `README.md`'s existing "branch on `kind` before
reading any other field", restated for a channel that adds a second, weaker
signal alongside it.

### The rule for a kind this table does not list

A later slice will add a variant. Pick its status by asking, in order, what the
caller can *do*:

1. Could a different request body have worked? → **400**
2. Does the thing addressed not exist? → **404**
3. Would this same request succeed later, once state moves? → **409**
4. Is it permanently impossible against this store's structure, no matter how
   long you wait? → **422**
5. Did quipu itself break? → **500**

Never a status outside `{400, 404, 409, 422, 500}` without adding a row above
and a reason. Never `200` for a failure. Never `5xx` for something the caller
caused.

### 503 is reserved and the server must never send it

`unavailable` means *no store saw the request*. A 503 written by the quipu
process is proof that a front door answered, which contradicts the kind. If the
process is up but cannot serve the named project, that is `no_store` (404); if
it is up and broken, that is `internal` (500).

`unavailable` is therefore synthesised **client side**, in exactly two
situations: the connect or read failed outright, or a `502` / `503` / `504`
came back — which, by the paragraph above, can only have been written by
something in between, such as a reverse proxy. Both mean the same thing to the
caller and both are exit 7.

## 3. Verb triage

Every command in `src/cmd/`, in exactly one bucket. The fan-out slice should
read this as a checklist.

### Served — writes (15)

`add`, `assign`, `claim`, `take`, `complete`, `block`, `cancel`, `abandon`,
`reclaim`, `log`, `tag`, `relation add`, `relation rm`, `depends`, `contains`,
`edit`.

All of these open the store, run one `with_tx`, and emit an `Outcome`. Nothing
in any of them touches process-local state (see the finding on `claimref`
below, which is about latency rather than correctness).

### Served — reads (8, plus one nested)

`list`, `show`, `tree`, `timeline`, `wave`, `status`, `report`, `decisions`,
and `relation list`.

These do not use `Outcome` — they serialise their own projections — so they
need their own response shapes, but the same envelope rule: bare object (or
array) on 200, `{"error": ...}` otherwise.

### Local-only — never served (8)

Each with the reason it can never be served, because "acts on the filesystem"
is not by itself an argument (so does every served write; the question is
*whose* filesystem).

- **`init`** — the only creator of a store, and it creates a file at a path on
  *this* machine. A served `init` would be "create a file on the server's disk
  at a path the request names", which is an arbitrary-file-write hole with a
  friendly name. Stores are created locally and then made reachable by name;
  the server picks up an existing file, it does not conjure one.
- **`install-skills`** — writes into this machine's agent-harness skill
  directory. Serving it installs skills on the server host, which is never what
  the caller meant.
- **`project`** (`register` / `list` / `lookup` / `unregister`) — the registry
  maps names to paths *on this machine*. A served answer would name paths that
  do not exist on the caller's disk. `main.rs` already treats these three as
  store-independent, exempting them from the attach check so they work with no
  store at all; that exemption is the same fact from the other side. The
  server *reads* this registry to resolve its own `--project`, but the verb
  stays client-side.
- **`remote`** (`set` / `show` / `clear` / `doctor`) — `doctor` exists to
  answer "can *I*, from here, reach this endpoint", and an answer from the
  server's network position is the wrong answer to that question; it also exits
  5 directly. `set` and `show` are defensible later additions, but splitting a
  subcommand group across the boundary is worse than keeping it whole, so the
  whole subtree stays local.
- **`export`** — takes `--out <DIR>` and writes JSONL batches there. Served,
  the batches land on the server's disk and the caller gets an empty directory.
- **`import`** — takes `--from <DIR>` and reads the files there. The mirror
  image: the caller's files are not on the server.
- **`sync`** — `export` plus `import` plus object-store network I/O, reading
  remote config and this process's credentials from its own environment. Every
  one of those is process-local.
- **`ledger`** — reads the shared object store over the network with this
  process's credentials, for the same reason.

A test the fan-out slice can apply mechanically: **if an `Outcome`'s fields name
a filesystem path, ask whose disk it is on.** `init`'s `db_path`, `project`'s
`path` and `registry`, and `export`'s `dir` and `file` all do, and all four
commands are in this bucket. That is a consistency check, not a coincidence.

### Client-side poll (2)

- **`wait`** — a 500 ms loop over a `COUNT` query, exiting 3 on timeout and 4
  on an empty cohort. It becomes a client-side loop over the served read
  endpoint that backs `list`. The exits stay in the client, so codes 3 and 4
  keep their meaning and never need HTTP statuses at all. (`wait` is the only
  place in quipu that exits from inside a command module rather than through
  `main.rs`. Over the wire that stays true, and stays local, which is why it
  needs no envelope change.)
- **`watch`** — a 500 ms tail of the event log by `--since <id>`. It becomes a
  client-side loop over a served events endpoint carrying `since`.

For the record, since it has been recorded wrongly more than once: **`watch`
contains no `std::process::exit` call.** The only command module that exits
directly is `wait`, in three places. Neither command is semantically broken
over HTTP — both are *already* polls, and moving the poll from a local file to
a read endpoint changes latency, not semantics.

Neither should become a streaming endpoint under this contract. Server-sent
events or long-polling wants an async runtime, and CLAUDE.md forbids one.

## 4. Collateral state changes

**Decision: any served verb whose transaction writes a `state_change` for a
task other than the one the caller named MUST report those changes in a
reserved `_effects` key on the success object.**

This is a general rule about the envelope, not a patch for one command.

### Why it is needed at all

Locally the omission is invisible: the caller's next read hits the same file and
sees the truth. Over the wire that is still true — the store is still one SQLite
file and a follow-up read is still correct. The problem is not that remote state
is wrong; it is that the caller is never *told*, so an orchestrator that
dispatches on "what just became ready" has to poll for an answer it could have
been handed. That is a large part of why `wait` and `watch` exist.

`qp depends` sets the precedent by reporting `promoted` — but it under-reports,
and the shape of the under-report is instructive: `refresh_ready_logged` returns
the full `Vec<i64>` of promoted ids, and `depends` reduces it to
`promoted.contains(&task_id)`, a bool about the named task. Every other task the
same call promoted is discarded. `complete` discards the vector outright.

### The shape

```json
{
  "display_id": "QP-7",
  "state": "done",
  "decisions": [],
  "artifacts": [],
  "_effects": [
    {"kind": "state_change", "task": "QP-9",  "from": "pending", "to": "ready",   "via": "complete"},
    {"kind": "state_change", "task": "QP-12", "from": "ready",   "to": "pending", "via": "block"}
  ]
}
```

Rules:

- `_effects` is present **iff** the transaction moved a task the caller did not
  name. Absent, not `[]`, when there was none — matching the existing
  `skip_serializing_if = "Option::is_none"` convention.
- The leading underscore marks the key as belonging to the *envelope* rather
  than to the verb. Every future envelope-level key takes the same prefix, so a
  verb author can never collide with one. This matters concretely: the obvious
  unprefixed name, `promoted`, is already taken by `depends` and already means
  something narrower.
- `task` is the canonical `display_id`, never the raw row id — the same rule
  the rest of the envelope follows.
- The key is added to the CLI's `--json` output too, not only to the wire.
  There is one shape; it is additive, so no existing consumer breaks.
- `human()` is unchanged by this contract. `_effects` is a serialised-form
  field. Whether the prose line should mention promotions is a real question and
  a reasonable later change, but if it is made, `human()` must read this same
  field rather than recompute anything.

### Which verbs this touches

Wider than `complete`. Every one of these already moves tasks the caller did not
name, and every one currently discards the information:

| verb | via | what it does with the ids today |
|---|---|---|
| `complete` | `refresh_ready_logged` | discards |
| `cancel` | `refresh_ready_logged` | discards |
| `contains` | `refresh_ready_logged` | discards |
| `abandon` | `refresh_ready_logged` | checks only whether the *named* task returned |
| `reclaim` | `refresh_ready_logged` | checks only whether the *named* task returned |
| `depends` | `refresh_ready_logged` | reduces to a bool about the named task |
| `block` | `demote_frozen` | discards — and these are demotions, the other direction |
| `add` | `reconcile_readiness` | discarded inside `db.rs`, before the caller sees them |

Two consequences the fan-out slice must budget for:

1. **`reconcile_readiness` returns `()`.** It calls `demote_frozen` and
   `refresh_ready_logged`, both of which return `Vec<i64>`, and drops both. It
   has to return them, which also changes `link_dep`, its only non-`add` caller.
2. **These are raw row ids, not display ids.** `_effects` needs an
   `i64 -> display_id` resolution step that none of these call sites performs
   today.

## 5. Wire identity

**Decision: the agent identity travels in the envelope as an explicit request
field. It is never derived from the connection.**

`--as <agent>` maps to a top-level `agent` field in the request body. It is not
derived from the socket, the source address, the OS user, or a TLS peer
certificate.

Reasoning: connection-derived identity would fix one identity for the life of a
connection, and the dominant deployment is the opposite of that. A wave
coordinator is a single process that claims, assigns, completes and reclaims *on
behalf of* many different agent ids. Deriving identity from the connection makes
that impossible without one connection per agent, which defeats the
one-connection-per-store design outright. Carrying it in the body also preserves
the CLI shape exactly: `--as` becomes `agent` and nothing else moves.

### This is unauthenticated today

Say it plainly: `--as` is unvalidated free text, so any caller that can reach
the port can act as any agent. That is not a regression — any caller that can
write the SQLite file can already do the same. But it means a server bound to a
non-loopback interface is a strictly larger attack surface than the file it
fronts, which is why the default bind must be loopback (see "TLS and loopback").

### The shape that keeps authentication additive

Two concepts, never one field:

- The **asserted** identity — `agent`, from `--as`, in the body. Exists today.
- The **authenticated principal** — a transport-level credential, an
  `Authorization` header. Does not exist today.

When authentication arrives, it arrives as the second, plus a policy relating
principal to the `agent` values that principal may assert. Because `agent` is
already a body field and the principal will be a header, that addition is purely
additive: no existing field changes meaning, and an unauthenticated server
simply has no principal to check against.

The one rule that keeps this true, and that a later slice will be tempted to
break: **never infer `agent` from anything but the explicit field.** A server
that helpfully defaults `agent` to the connection's OS user is fine today and
breaks the day auth lands, because the convenient default and the enforced
principal will disagree, silently, in favour of whichever is checked second.

## 6. Human output ownership

**Decision: the server returns the structured payload; the client renders
`human()`.**

Three reasons, in order of weight:

1. `Outcome` exists so the JSON and the prose cannot drift apart, because they
   are two views of one struct. Rendering prose server-side puts the sentence on
   the wire and makes the JSON a separately-produced second thing — exactly the
   failure the trait was introduced to prevent.
2. Otherwise the server must know the client's `--json` mode, which puts a
   terminal presentation flag into the protocol. `--json` is a property of the
   invoking terminal and has no business crossing a socket.
3. The response is then byte-identical whether the user asked for JSON or prose:
   one shape, one thing to test.

### The consequences, checked

This is where a clean decision becomes a rewrite three slices later if nobody
looks, so all of the `Outcome` impls in the tree were read. What is there:

**Good news, and worth stating because it was not obvious: every `human()`
implementation is a pure function of its own struct's fields.** No `Connection`,
no path lookup, no environment read, no I/O of any kind, across all of them. The
decision is implementable as stated.

**The cost, which is real: every `Outcome` struct derives `Serialize` only —
never `Deserialize` — and every one is private to its `cmd` module.** The client
cannot reconstruct a payload to call `human()` on it. Making them
round-trippable means `#[derive(Serialize, Deserialize)]` and `pub` (or a
`pub(crate)` re-export) on every one, plus their nested types: `take`'s inner
task, `ledger`'s per-project and per-task rows, `sync`'s embedded export and
import outcomes, `import`'s anomaly list. It is mechanical, it is spread across
every command module, and it is the single largest hidden cost in this design.
Budget for it in the vertical slice rather than discovering it in the fan-out.

**A subtlety that will bite during that change:**
`#[serde(skip_serializing_if = "Option::is_none")]` — `add`'s `Created` uses it
— omits the key entirely, so deserialisation needs `#[serde(default)]` on those
fields or it fails on a payload the server considers perfectly valid. The same
applies to every optional field added later.

**A retracted finding, left here because the retraction is the useful part.**
This section originally reported that `remote`'s payloads smuggled pre-rendered
prose through a `checked: String` that `human()` split with `.lines()`, and
called it the anti-pattern this decision exists to forbid. That was read from an
older shape. The field is a struct of typed reachability results, and `human()`
*renders* prose from those variants rather than replaying a stored blob — which
is exactly the arrangement this decision prescribes.

The distinction is worth keeping, because it is the one a reader has to make on
every payload: a formatting function over structured data is correct no matter
which side runs it, and a `String` that already contains the answer is not. Only
the second forces the server to render, and only the second cannot be re-rendered
differently by a client that wants to.

## 7. TLS and loopback

**Decision: the server URL follows the same rule `s3.rs` already applies to
object-store endpoints, reusing `is_loopback_literal` rather than
reimplementing it.**

- `https` is required for any host that is not a loopback literal.
- `http` is permitted only when the host *is* a loopback literal —
  `127.0.0.0/8`, `::1`, or the literal `localhost` — and plaintext is explicitly
  requested, the same shape as `scheme_for(host, plaintext_requested)`.
- A name that merely *resolves* to loopback does not count. That is
  `is_loopback_literal`'s existing behaviour and it matters more here than for
  object storage: a DNS answer is attacker-influenceable, so "resolves to
  127.0.0.1 right now" is not a security property.

Reuse rather than reinvention, because there should be exactly one place in this
codebase that decides when plaintext is acceptable, and because the second
implementation is the one that gets the hard case wrong. `host_without_port`
handles bracketed IPv6 specifically so that `::1` does not split on its last
colon into an empty host that parses as nothing and is then treated as a real
remote endpoint. That comment reads like someone found out the hard way.

**Separately: the default bind address is loopback.** The URL rule governs the
client; the bind governs the server, and both are needed. A server bound to all
interfaces is reachable by anything on the network and is unauthenticated
(section 5).

**Boundary: no TLS stack is chosen or implemented here.** This rule constrains
what a URL may be. Terminating TLS is the vertical slice's problem, and
"terminate it in a reverse proxy that talks plaintext to loopback" is a
legitimate answer — in which case `qp` speaks `http` to `127.0.0.1` and the rule
is satisfied exactly as written.

## Still open

One thing this contract deliberately does not do, recorded here so it is not
lost: **the leanness rule in `CLAUDE.md` still says "zero daemons", and it has
not been amended.** That line must be amended, with reasoning, in the same
commit that first introduces a server — the way the binary-size rule was — which
is the vertical slice's job, not this one's.
