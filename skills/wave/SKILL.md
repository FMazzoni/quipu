---
name: qp-wave
description: Execute a wave with qp. Plan tasks, workers pull with take, adversarial review, loop until clean. Works against a local store or a front door.
---

## When to use
Independent work units that can be parallelized, with a mandatory adversarial-review step.

Two deployments, one recipe. **One machine:** the coordinator and its agents all
open the same store file, and nothing below needs a server. **N machines:** one
`qp serve` process fronts the store, and every worker reaches it over HTTP. The
commands are the same either way — that is the point of the front door — so the
recipe is written once and the differences are confined to "Reaching the board".

## Adversarial review — mandatory

- After a wave lands, before you commit: spawn a dedicated adversarial agent over the diff. Inline self-review does not substitute.
- Gates (lint, types, tests) are necessary but insufficient — the failures that survive them pass every check, exit 0, and are silently wrong: a field wired in but read by nothing, an error path that returns the success value.
- Order is fixed: **land → gates → adversarial pass → fix → commit** — never commit-then-review.
- Brief it with the diff range, what the code is *for*, and the known failure modes. It ranks findings, separates "silently wrong" from "untidy", verifies every claim against the code rather than the implementer's report, and only proposes — you apply.

## Reaching the board

A worker needs two things: where the front door is, and which project on it.
**Two environment variables and no local store is a working machine.**

```bash
export QP_SERVER=http://127.0.0.1:8787   # or https://<terminator> from another host
export QP_PROJECT=<project-name>         # only when the front door serves more than one
qp list
qp take --as worker-1
```

That machine has no `.quipu` anywhere, and running the above leaves nothing
behind on its disk. There is no `qp init` on a worker: `init` creates a store
file *here*, and a worker is precisely the machine that should not have one.
The store is created once, on the machine that holds it, and fronted by name.

**Do not configure a worker with `qp remote set --front-door`.** That records a
url in a *store*, so it cannot help a machine that has none — the limitation is
deliberate and is written down in the README and in `docs/modules/remote.md`.
What that command is for is the machine that *does* hold the store: a
coordinator that wants its own commands to go through the same front door its
workers use, so one board is one board rather than a file and a socket.

Precedence, when more than one is set: `--server` on the command line, then
`QP_SERVER`, then the url the store recorded, then the store on this disk.

### What is not served, and it matters here

`qp wait` and `qp watch` **are not served.** Both are poll loops and both still
open the store on the caller's own disk; with `--server` set they print
`warning: --server is set but this verb is not served yet` and answer from that
local file anyway. On a worker with no store that is exit 6; on a coordinator
that also has a local store it is worse, because it answers — from the wrong
board, quietly, while every other command goes over the wire.

So **do not use `qp wait --cohort-done` as a wave barrier when the board is
behind a front door.** Poll a served read instead; `wave` and `list` are both
served:

```bash
# Barrier: block until the tagged cohort has no non-terminal tasks left.
until [ "$(qp list --tag wave:$WAVE --state ready --json | jq 'length')" = 0 ] \
   && [ "$(qp list --tag wave:$WAVE --state assigned --json | jq 'length')" = 0 ] \
   && [ "$(qp list --tag wave:$WAVE --state running --json | jq 'length')" = 0 ]; do
  sleep 2
done
```

Against a purely local store `qp wait --tag wave:$WAVE --cohort-done` is still
the shorter and better barrier, and its exit 4 on an empty cohort still catches
a typo'd tag that the loop above would read as instant success. Use it there.

`init`, `install-skills`, `project`, `remote`, `export`, `import`, `sync` and
`ledger` are local-only too, for reasons that are about *whose* disk or *whose*
network position is meant. None of them belongs in a worker's loop.

## Conventions

- **Wave id:** every task in a wave is tagged `wave:<id>`.
- **Critique tasks:** tagged `kind:critique`, `--depends-on <reviewed-task>`.
- **Non-blocking findings:** add `blocking:false` tag if the critique should not gate progress.
- **Variants (branch-and-evaluate):** use `qp relation add <T> variant-of <root>`. After picking a winner, `qp cancel` losers.
- **Decisions:** agents log autonomous decisions via `qp log QP-<n> decision "..." --as <agent> --auto`.

## Recipe

```bash
WAVE=$(date +%s)
qp init   # idempotent — on the machine that holds the store, never on a worker

# 1) Plan
qp add "impl X" --tier wave-$WAVE --tag wave:$WAVE
qp add "impl Y" --tier wave-$WAVE --tag wave:$WAVE
# ...

# 2) Hand out work. Two shapes; pick one, do not mix them in one wave.
#
#    PULL (default, and the only one that works for N independent machines):
#    each worker takes its own next ticket. Selection and assignment are ONE
#    guarded statement, so two machines can never take the same ticket — the
#    loser is refused, not corrupted. There is nothing for the coordinator to
#    do in this step at all.
#
#    PUSH (`qp assign`): only when the coordinator must decide WHICH agent gets
#    WHICH ticket — a slice that needs a particular worktree, say. It is a
#    deliberate choice, not the default.
#
#    Never `qp list --state ready` followed by `qp assign`: that is a
#    read-then-write across two processes with a window in the middle, and it is
#    exactly what `take` replaced.

# 3) Each worker runs, in its own loop:
while t=$(qp take --as "$AGENT" --tag wave:$WAVE --json | jq -r '.taken.display_id // empty'); [ -n "$t" ]; do
  qp claim "$t" --as "$AGENT"     # assigned → running, and this is what takes the lease
  # ... do work ...
  qp complete "$t" --as "$AGENT" --decision "result summary"
done
# An empty queue is exit 0 with {"taken": null} — a drained wave is not an
# error, and branching on the exit code would turn every idle worker into a hot
# loop. Branch on the payload, as above.
#
# `take` stops at `assigned`; the claim is a separate edge and a separate
# command, because the lease belongs to the claim.

# 4) Wait for the wave to drain. See "What is not served" above: `qp wait
#    --cohort-done` locally, the polled `qp list` barrier behind a front door.

# 5) Adversarial pass — MANDATORY, before committing the wave (see top of file).
#    Spawn a read-only adversarial agent over the wave's diff range. For each
#    surviving finding, file a task:
#    qp add "fix: <finding>" --depends-on <reviewed-QP-N> --tag wave:$WAVE --tag kind:critique
#    Verify the adversarial agent's claims yourself before acting -- it can be wrong
#    too; today one "eliminates it at zero cost" claim held, another's supporting
#    detail did not, though its recommended fix was still right.

# 6) Promote-and-loop: critiques that exist are already tasks. Workers on the
#    pull loop in step 3 pick them up with no coordinator action, because a
#    completed dep thaws its dependents back to `ready` automatically.

# 7) Reclaim orphans — a worker whose machine died without completing.
# Only the STALE ones: a hold whose lease has lapsed. `qp reclaim` releases a
# stale hold with no flag and REFUSES a live one, naming the holder, the machine
# and when it expires, so reclaiming everything `running` would fail on the
# agents still working — which is the point.
for t in $(qp wave --json | jq -r '.running[] | select(.stale) | .display_id'); do
  qp reclaim $t --reason "post-wave cleanup"
done
```

## Leases: what "stale" means and what it does not

A claim carries a lease, and **the lease bounds silence, not duration.** Every
write by the holder moves its clock forward, so a slice that runs for three days
while logging against its ticket never goes stale; a slice whose machine died
five minutes ago does. A long-running worker is not at risk. A silent one is.

Nothing sweeps. There is no daemon and no timer promoting expired holds —
staleness is evaluated when someone *reads*, so a hold that lapsed an hour ago
simply reads as stale the first time anyone looks. `qp show <id>` and
`qp wave --json` both carry it: `hold.stale`, alongside the holder, the host and
the expiry instant.

That is what makes a stranded ticket visible instead of indistinguishable from
work in progress, and it is why step 7 filters on `stale` rather than on
`running`. `--force` exists for a live hold and should be rare: reaching for it
means overriding a lease that says a machine is still there.

A ticket sitting `assigned` and never claimed has no holder and needs no force —
nobody had started.

## When a write fails against the front door: read, then decide

**Exit 7 (`unavailable`) means UNKNOWN OUTCOME, not "did not happen".** The
front door could not be reached — but if the request was sent and the response
was lost on the way back, the transaction may have committed microseconds before
the connection died. Nothing anywhere in quipu retries, so this is the caller's
to resolve, and there is nobody else to resolve it.

The sequence, in this order:

1. **Do not blindly re-issue the write.** It may be a second attempt at
   something that already succeeded.
2. **Read before writing.** `qp show <id>` answers "did my write land", and
   `qp timeline <id>` is *exact evidence*: the event insert runs inside the same
   transaction as the mutation, so the absence of the event is positive proof
   the transition never landed.
3. **Re-issue only if the read says it did not land.** After the read it is an
   ordinary request with an ordinary exit 0 or 2.

Two things agents get wrong after a server restart:

- **A claim committed before the crash is still held.** It is a row, not a
  server-side session; nothing about it lived in the process that died. An agent
  that assumes its claim evaporated and re-claims gets exit 2 `already_claimed`
  naming *itself*.
- **The lease kept running.** Expiry is wall-clock against a stored timestamp
  and does not tick in the server, so downtime is not paused time. A long outage
  can expire a lease — and the recovery is the one that already exists, `qp
  reclaim` or lease expiry, exactly as for a worker that crashed. A server
  restart is not a new failure mode; it is the crashed-agent failure mode
  arriving from a different direction.

**There is no offline mode and no cache.** With a front door configured and
unreachable, *reads* fail exactly like writes — exit 7, nothing served from a
stale copy. That is deliberate: a cached `ready` cohort answers the dispatcher's
one question with a snapshot of what *was* free, and it dispatches work another
machine already holds. A loud failure beats an answer you cannot date. The way
back to a local board is `qp remote clear --front-door`, a deliberate act with
an event behind it — and it works while the front door is down, because
`remote` is not served.

Exit codes do not change behind a front door: a lost race is exit 2 whether the
store was on this disk or behind a socket. Only exit 7 is new.

## Observability during a run

- `qp wave` (one-shot): grouped `{ready, assigned, running, pending}` with `stale` on each hold. Served.
- `qp show QP-<n>` / `qp timeline QP-<n>`: everything that happened to one task. Served.
- `qp decisions`: skim autonomous decisions after the wave. Served.
- `qp list --tag wave:$WAVE`: everything in this wave. Served.
- `qp watch`: live event stream in a side terminal — **local store only**, so run it on the machine that holds the store, not on a worker.

## Variants (branch-and-evaluate)

```bash
ROOT=$(qp add "explore approach" --tag wave:$WAVE --json | jq -r '.display_id')
for v in a b c; do
  T=$(qp add "try-$v" --tag wave:$WAVE --json | jq -r '.display_id')
  qp relation add $T variant-of $ROOT
done
# ...dispatch, evaluate...
WINNER=QP-<x>; for L in $(qp relation list $ROOT --json | jq -r ".incoming[].from" | grep -v ^$WINNER$); do
  qp cancel $L --reason "superseded by $WINNER"
done
qp relation add $WINNER supersedes $ROOT
```

### Blocker pattern (deps-as-blockers)

When a running task hits an obstacle, the agent doesn't mark it `blocked` (that state no longer exists). Instead, it creates a blocker task as a new dep:

    qp block QP-3 --as wave-N:agent-a --new "DB schema migration needed for QP-3"

This is shorthand for:

    qp add "DB schema migration needed for QP-3" --tag kind:blocker  # QP-9
    qp depends QP-3 --on QP-9 --as wave-N:agent-a
    qp abandon QP-3 --as wave-N:agent-a   # demoted to pending due to new dep

`kind:blocker` is this skill's convention, not a substrate rule — it is only the default of `qp block --tag`. Another orchestration pattern passes its own (`--tag kind:review`, repeatable; supplying any replaces the default). Nothing in the binary reads the tag back: `qp wave` classifies a task as blocked from its unresolved dep edges alone, so the tag is purely a `qp list --tag` filter handle.

The blocker appears in `qp wave` under `ready`, and on the pull loop the next
free worker takes it with no coordinator action. When it completes,
`refresh_ready` thaws QP-3 back to `ready` and a worker takes that too.

Exploratory planners use the same primitive *without* `qp block` — they just call `qp depends parent --on child` to push planning work down the DAG.

### Editing tasks

Task title, tier, and description are editable after creation:

    qp edit QP-3 --title "fetch staging credentials" --description "see Linear INGEST-42"

Each call emits one `edit` event capturing the before/after of every changed field. No-ops are silently skipped (no event). State, display_id, and created_at are not editable. Edits are allowed in any non-terminal state — including `running` — for scope-refinement mid-flight.

`--as <agent>` is optional and only used for attribution in the `edit` event payload.
