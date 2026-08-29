The synopsis is fenced deliberately: outside a code block, rustdoc parses
`<url>` and friends as unknown HTML tags and silently deletes them.

```text
qp remote doctor [--json]
```

## Why this exists

Every setup failure this project has met presented as something other than its
cause. A key that is perfectly good is reported as rejected. A bucket that is
there is reported as missing. A host that is up is reported as down. In each
case the mechanism was two commands once you knew which two — the cost was
DISCOVERY, and it was paid in hours.

So this command's product is not a pass/fail. It is a NAME for the failure, and
a remedy that acts on that name. A diagnosis nobody can act on is a slower error
message.

## Stop at the first failure

Within one sequence the checks run in order and the sequence stops when one
fails; the rest are recorded as skipped. This is the rule that makes the output
worth reading. A missing credential makes every later check answer a question
nobody asked, and a screen of red herrings is the experience this command exists
to replace. The sequences are independent of each other: a broken ledger never
suppresses the naming checks, and never suppresses the front door's.

Nothing configured is not a fault — local-only is the default and a supported
state, and so is a store with only one of the two urls set.

A skipped check says an earlier one in ITS OWN sequence did not pass. "Did not
pass" rather than "failed", because a note stops a sequence too: `registered` is
advice on the ordinary unregistered store, and the `entry_path` skip that
follows it used to claim a failure that had not happened — on almost every run.

The ORDER inside the ledger sequence is itself a diagnosis. The clock is checked
before the credential verdict, because a clock a quarter of an hour out makes the
store refuse a key that was never wrong — and "the credentials were rejected" is
precisely the answer that sends someone to rotate it.

## Where a check cannot tell two causes apart, it names both

Some pairs are genuinely indistinguishable from one request, and picking one
silently is how an afternoon gets spent on the wrong fix:

- **A missing bucket versus a key not scoped to it.** Both arrive on the same
  call and want opposite fixes — create the bucket, or change the policy. When
  the url carries a prefix, a second LIST at the bucket ROOT separates them: a
  root that lists proves the bucket exists, so the fault is the policy; a root
  that answers `NoSuchBucket` proves the opposite. When the root is denied too —
  which is what a store does for a bucket you may not see, whether or not it is
  there — the report says so and names both.
- **Nothing listening.** The service being down, an address left behind when a
  host moved onto a private mesh network, and this machine being locked out of
  that network produce byte-identical refusals. All three are named. The front
  door's version of the same list is its own three causes: no `qp serve` is
  running, one is running elsewhere, or this machine cannot reach that host.

- **A front door that is absent versus one that is alive and serving other
  projects.** These want opposite fixes — start a process, versus restart a
  running one with a different `--project` set — and one is not even a fault of
  the server's. They are separate checks, `reachable` and `serves`, and the
  second only runs when the first passed.

## The object-store probe writes, and does not delete

The write probe PUTs a fixed, flat key and reads it back. Fixed because quipu's
object client has no DELETE at all — nothing in the ledger is ever deleted, so
requiring a delete permission would fail stores whose key is correctly scoped
without one. A fixed key is overwritten by the next run instead of accumulating.
Flat — no `/` — because a key containing one would appear as a *project* to the
listing that reads `CommonPrefixes`.

PUT and GET are separate checks because a policy can grant one and not the other,
and sync needs both: it writes its own batches and reads everyone else's.

## Output and exit code

Human-readable by default, `--json` alongside. Every check carries a stable
`name` so a setup script can branch on one, and every failure carries a `remedy`.
The diagnosis goes to stdout in the requested format whatever it finds — the
command did what it was asked — and the exit code is the signal: `0` when nothing
failed, `5` when something did. A code of its own rather than the retryable
conflict code, so a skill that retries on that one does not loop forever against
a remote that will never come good.

## The front-door sequence

`qp remote doctor` could not diagnose a front door at all, and the way it failed
is the reason this sequence exists. Before the change, `qp remote set --ledger
http://...` was refused outright — correctly, and with an accurate message
naming the `s3://` form — so a server url could not be stored; forced into
`meta` by hand, it was reported as a malformed LEDGER and the remedy told you to
re-set it as object storage. Accurate about the ledger, useless about the thing
you actually configured.

The checks, in order, and the order is the diagnosis:

1. **`url`** — it is an http(s) url a client would call, and plaintext only to a
   loopback literal. An `s3://` url here is a url fault and stops the sequence:
   probing it would report a front door as unreachable, which is a confident
   wrong answer wearing a new costume.
2. **`reachable`** — one `GET /healthz`. Two distinct failures share this check
   and carry different remedies. Nothing answered: the three causes above, plus
   the standing fact that `qp serve` is never started implicitly, so a url that
   was set is not a process that exists. Something answered and it is not a
   quipu front door: another service holds the port, or a proxy is not routed —
   and the remedy must NOT be "start a server", which would mean starting one on
   a port already taken.
3. **`version`** — see below. Never a failure, and placed before `serves` for
   the reason the ledger checks the clock before the credential verdict: it is a
   property of the process rather than of the request, so knowing it colours
   everything reported after it.
4. **`serves`** — is this store's project name in the list the process opened at
   startup? A failure here is NOT unreachable and never transient: the store set
   is fixed before the listener binds and no request can add to it, so the
   remedy is a restart naming the project, or a different front door.

The probe asks nothing the health endpoint refuses to answer. `/healthz` takes
no project parameter by contract — a liveness check whose answer depends on user
input makes "unknown project" look like "process unhealthy" — so the list comes
back whole and the comparison happens on this side.

When the store has no readable project name, `serves` is a NOTE naming what the
front door does serve, not a failure. The `project` sequence has already failed
on the name, and reporting it a second time as a server fault would name the
wrong thing to fix.

## Version mismatch: the check that would have been worse than the problem

`/healthz` already reports `CARGO_PKG_VERSION`, so the comparison is buildable
and is built. What it is NOT is a gate: a difference is a NOTE with advice, and
it never fails the run.

The reasoning is that the crate version is neither necessary nor sufficient for
the thing that actually has to match. What a request depends on is the served
verb table, the error taxonomy and the payload shapes — and two builds of one
version from different commits can differ in all three, while two adjacent
versions normally differ in none. A `Fail` here would fire on the ordinary case
of an operator upgrading their client before restarting the front door, and a
check that fires on the ordinary case is one people learn to ignore. That is the
same rule that keeps "not registered" a note.

So the `Pass` is deliberately hedged in its own text — equal versions are not
proof of an equal verb table — and the `Note` says what to watch for instead: a
verb the front door should serve coming back `not_found`, or a payload field
this client expects being absent. Those are observations about the contract
rather than about a version string, and they are what a real compatibility check
would test.

**What a useful hard check would need, and why it is not here.** It would need
`/healthz` to report the verb table it serves, so a client could compare the
verb it is about to send against the set the server answers. That is a widening
of a published endpoint, fixed by the process contract, and it belongs to
whoever changes that contract — not to a slice adding a diagnostic. Until then,
`not_found` on a verb the client believes is served is the honest signal, and it
arrives at the moment it matters rather than on a doctor run.

## Diagnosing a url, on a machine that has no store

The command that says "you are misconfigured" could not run on the machine most
likely to be misconfigured. A worker holding the binary, `QP_SERVER` and
`QP_PROJECT` and nothing else — the deployment the front door exists for — got
`no quipu store in … or any parent` and exit 6. It could list the board and take
work; it could not diagnose itself.

**The obvious fix is exempting `remote` from the attach check, and it does not
work.** `db::open` calls `require_store` as a backstop, so the exemption moves
the same exit 6 one function down and replaces a cwd-aware message with one
naming an internal path. That was reproduced before this was designed.

**And the exemption is the wrong shape anyway**, which is the part worth
keeping: on a storeless machine there is no recorded url to diagnose. What that
machine wants diagnosed is the url in its environment. So this is a doctor that
diagnoses **a url**, not a doctor that diagnoses a store and happens to reach a
url — which is what `Subject` names.

### The surface is the flag that was already there

No new flag. The global `--server` (and `QP_SERVER` behind it) already names a
front door for one invocation, and `--project` / `QP_PROJECT` already names the
project. `qp remote doctor` diagnoses those, and the precedence is
`resolve_server`'s own — the url named for this invocation first, the store's
recorded url after it — rather than a second order invented here. A machine that
has both gets the one its other commands would actually use, and the
disagreement is reported as a `config` note rather than a fault: naming a front
door on the command line is how an operator points a client somewhere else.

`qp remote doctor` inside a project directory is unchanged, and that is a
constraint rather than a hope: the store-side sequences are the same checks in
the same order, and the twenty-five tests on them run untouched.

### What runs with no store, and what cannot

| sequence | storeless |
| --- | --- |
| `config` | runs — a storeless client with a front door is a **`Pass`** |
| `registry` | runs — the registry is a machine-local file, not a store's |
| `project` | runs in a storeless form: `name` is a note, `name_form` is really checked, `registered` and `entry_path` are not applicable |
| `ledger` | not applicable — the ledger url is a row in a store's `meta` |
| `server` | runs, and is the whole point |

**Not-applicable is not a skip, and the difference is the whole of what makes
this readable.** `Seq::skip` says an earlier check in this sequence did not
pass; saying that about a machine that was deliberately built without a store
would report the intended configuration as damage. `Seq::not_applicable` says
there is nothing here to look at. Both land on `Status::Skipped`, so a script
that branches on the status keeps working, and the detail is what tells them
apart.

The one store-side check that survives the loss of the store is `name_form`,
because the project name is not really a store's property here: it is a shell
argument and the front door's lookup key, and a spelling that normalises two
ways selects nothing on either machine.

### No local store reads as healthy

A storeless client is first-class, exactly as local-only is, and the report says
so in the `config` check rather than merely declining to complain. The reason to
be deliberate about it: a diagnostic that calls the intended configuration
broken is worse than no diagnostic — people stop running it, and then it is not
there on the day something is genuinely wrong.

The one storeless state that IS a fault is having neither a store nor a url:
nothing to work against and nothing to diagnose. The CLI answers that with
`require_attached`'s cwd-aware message before this module is reached — the
exemption is scoped to "a front door was named" for exactly that reason — so
the branch here is pinned by a unit test rather than an end-to-end one.

### What a storeless client changes in the front-door sequence

Two things, both of them in what the checks may assume:

- **`serves` has no store to take a name from.** It takes it from
  `--project` / `QP_PROJECT` instead, and the comparison is otherwise
  identical. Naming *no* project is not a fault on its own: an unnamed request
  selects a front door's only store, so it passes when the process serves one
  and fails when it serves several — which is a real misconfiguration and
  precisely the one that otherwise shows up as an empty board.
- **The unreachable remedy's tail differs.** The store-side version explains
  why `qp remote clear --front-door` is not the first thing to reach for; on a
  storeless client that command needs a store it does not have, so following it
  produces a second `no_store` on top of the fault. There, the url in the
  environment is the whole configuration, and the tail says so.

## Naming and registry checks

Two sequences run unconditionally, before anything that needs a remote: naming
and the registry are how a caller reaches this store *by name*, and they are
wrong in the same way whether or not a remote is attached.

The front-door sequence is independent of both, and of the ledger's. A store may
have a ledger, a front door, both or neither; each sequence runs iff its url is
configured, and none of them suppresses another. Local-only means neither url is
set, and it stays a note rather than a fault.

`registry` covers the file — readable, versioned, no name claimed twice, no entry
pointing at a store that is gone. `project` covers this store — it has a name,
the name is well-formed, it is registered, and the entry points back here.

They are two sequences rather than one because the faults are independent, and
each sequence stops at its first failure. A duplicate somewhere else in the file
says nothing about whether this store is named correctly; folding them together
would hide one fault behind the other.

Not being registered is a NOTE that carries a remedy, not a failure. Every store
starts unregistered and most stay that way — every command today finds its store
by walking up from the cwd — so failing the run there would fire on the normal
case, and a check that fires on the normal case is one people learn to ignore.
`Seq::advice` exists for exactly this: something to do about a state that is not
a fault. The genuine faults keep their exit code: a malformed name, a duplicate,
a dangling entry, and an entry that resolves to a different store.

The `project` sequence's `name` failure is a guard, not a routine diagnosis: any
open of a store heals a missing name, so a store reached through the CLI always
has one. It fires for a reader that does not heal — a store on a read-only
filesystem, or a process holding a connection it did not open — and its branch is
pinned by a unit test rather than an end-to-end one for exactly that reason.

## What used to be here

Roughly eight hundred lines diagnosing a git claim authority: transport-aware
classification of git's stderr, a namespace write probe under
`refs/quipu/doctor/`, and an ssh-multiplexing report. They went with the
authority itself — claims are arbitrated by one writer holding
`BEGIN IMMEDIATE`, so there is no remote in the claim path to diagnose. The
ledger checks, the registry checks and the project checks are unrelated to that
and all stay.
