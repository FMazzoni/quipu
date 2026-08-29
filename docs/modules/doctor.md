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
suppresses the naming checks.

No ledger configured is not a fault — local-only is the default and a supported
state.

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
  that network produce byte-identical refusals. All three are named.

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

## Naming and registry checks

Two sequences run unconditionally, before anything that needs a remote: naming
and the registry are how a caller reaches this store *by name*, and they are
wrong in the same way whether or not a remote is attached.

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
