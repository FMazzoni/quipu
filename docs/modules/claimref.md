A claim is only real once the authority says so. This module is the "says so".

## The order, which is the whole design

```text
1. no authority configured  -> local guarded UPDATE, exactly as before
2. authority configured     -> acquire the ref first
     git push --force-with-lease=<ref>: <authority> <blob>:<ref>
3. only on success          -> local guarded UPDATE
4. on release               -> delete the ref
```

Reversed, a store writes down a claim it does not hold, which is the single
failure this exists to prevent. When the local transaction refuses the claim
after the ref was acquired, the ref is given back (`Held::roll_back`) — the
acquisition is not allowed to outlive the attempt it belonged to.

Releasing is the mirror image and therefore the opposite order: local first,
ref second. A failure there leaves a ticket held by a store that no longer
wants it, which is recoverable. The other order would allow two holders.

## Fail closed

If the authority cannot be reached, the claim fails and nothing local is
mutated. There is no local-only fallback — a fallback would hand out exactly
the duplicate claim the authority exists to refuse. The sanctioned way out of
an unreachable authority is `qp remote clear --authority`, which is a
deliberate, recorded act, and the unreachable message names it.

## Three primitives that are not interchangeable

**The lease is not optional.** `--force-with-lease=<ref>:` with an *empty*
expected value means "this ref must not already exist", and the remote is what
verifies it. A plain push is not a weaker version of this, it is a broken one:
refs outside `refs/heads` and `refs/tags` accept any fast-forward, so a second
claimant whose object descends from the winner's is accepted with exit 0 and
takes the claim silently. A test that races two *unrelated* objects passes
against the broken primitive and proves nothing; the descendant case is the one
that has to be pinned.

**Every attempt mints its own object.** Git treats a push of the value a ref
already holds as a no-op and answers `Everything up-to-date` with exit 0. Ten
claimants pushing one shared anchor produced five winners. The blob carries a
nonce so no two attempts can collide, and exit 0 with `Everything up-to-date`
is read as a loss rather than a win.

**The claim object is a blob.** A commit carries the operator's name and email
out of git config — real identity, published to a third-party host — plus a
parent pointer whose chain can drag ledger objects along with the push. A blob
has no author, no committer, no tree and no parent.

## What the ref is keyed on, and why it is not the local name

The ref name is a digest over the ticket's **canonical cross-store identity**:
the uuid of the store that minted it plus the rowid it holds *there*, which is
`task_origin`'s `(origin_uuid, origin_task_id)`. A task with no row there was
minted here, so its canonical name is this store's uuid and its own rowid.

Nothing machine-local may go in, and this is the part that is easy to get
wrong. Peers are not replicas. Each keeps its own `project_uuid`, and display
ids are *local names* — the same logical ticket is `QP-1` on one machine and
`QP-3` on another, both correct. Keying the ref on the asking store's uuid or
on a display id makes two peers compute two different names, so they contend
over nothing and **both** exit 0 holding the same ticket. That was measured:
two independent stores, one authority, two refs, two winners, no warning. It is
the false-success failure the whole feature exists to prevent, and it is
invisible to any test whose second store is a copy of the first, because a copy
makes both inputs agree by construction.

The digest itself stays. It was never the problem; the inputs were.

## What crosses the wire

Ref names, opaque; the blob body, which is an agent id, a timestamp and a
nonce; and the timing and count of pushes. Ticket titles, descriptions, tags
and events are not reachable from a blob and never transmitted. Push rhythm is
irreducible — it is inherent to using a remote as a referee.

## Error classification

```text
exit 0 + "Everything up-to-date" -> lost: the ref already holds this object
exit 1                           -> lost: the ref exists, someone else holds it
exit 128 (or anything else)      -> no answer: unreachable, or auth refused
```

The 1-vs-128 split is mechanical and needs no output parsing, which is what
makes "already claimed" trustworthy. Splitting *unreachable* from *auth
refused* does need stderr matching, so it is best-effort and biased: a missed
match reports unreachable. A parse miss must never be able to turn "could not
reach the host" into "someone else holds this".

## Which edges release

`complete`, `cancel`, `abandon`, `reclaim` — and `block`, which is easy to
overlook because its name does not sound like a release. It is
`assigned`/`running` → `pending` like the other two release edges, so a `block`
that kept the ref would leave a ticket held by nobody and claimable by nobody.

## The ssh lane

`Arbiter` passes the acting agent's id to `remote::ssh_command` as the lane.
Measured: no multiplexing costs seconds per claim; one shared master with ten
concurrent pushes fails every one of them, because sshd counts the master and
its channels against `MaxSessions`. Keying by agent gives both halves —
sequential claims by one agent reuse a socket, N agents get N sockets. A
constant lane here makes a whole fan-out fail with errors that look like the
authority being down.

## Where the objects live

`<store-dir>/claims.git`, a bare repository quipu creates on first use and
owns. `git hash-object -w` writes into whatever object store it is pointed at,
and quipu has no business writing into a repository it does not own — nor any
guarantee it is being run inside one at all. A local-only store never creates
it.
