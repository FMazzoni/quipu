---
name: qp-wave
description: Execute a wave with qp. Plan tasks, assign in parallel, dispatch subagents, adversarial review, loop until clean.
---

## When to use
Independent work units that can be parallelized, with a mandatory adversarial-review step.

## The adversarial-review pass is mandatory, not optional

After **every** wave lands and before you commit or promote it, spawn a dedicated
adversarial agent over the wave's diff. This is not a nicety — passing the
project's own gates (linters, type checks, tests, whatever CI runs) is necessary
but **insufficient**. The failures that survive the gates are the dangerous ones:
work that passes every check, exits 0, and is silently wrong. A dedicated
adversarial read is the only thing that reliably catches them.

Do NOT substitute inline self-review. A wave reviewed only inline ships bugs an
adversarial agent would have caught — e.g. a config field wired in but read by
nothing, or an error path that returns the same value as success. Both pass every
gate.

The order is fixed: **land → run the project's gates → adversarial pass →
address findings → THEN commit.** Not commit-then-review.

Give the adversarial agent: the exact diff range (`git diff <base>..<head>`), what
the code is *for* (so it can judge severity), the project's known failure modes,
and an explicit instruction to rank findings and distinguish "would silently
produce a wrong result" from "untidy". Tell it to verify claims against the code,
not trust the implementing agent's report. Read-only; it proposes, you apply.

## Conventions

- **Wave id:** every task in a wave is tagged `wave:<id>`.
- **Critique tasks:** tagged `kind:critique`, `--depends-on <reviewed-task>`.
- **Non-blocking findings:** add `blocking:false` tag if the critique should not gate progress.
- **Variants (branch-and-evaluate):** use `qp relation add <T> variant-of <root>`. After picking a winner, `qp cancel` losers.
- **Decisions:** agents log autonomous decisions via `qp log QP-<n> decision "..." --as <agent> --auto`.

## Recipe

```bash
WAVE=$(date +%s)
qp init   # idempotent

# 1) Plan
qp add "impl X" --tier wave-$WAVE --tag wave:$WAVE
qp add "impl Y" --tier wave-$WAVE --tag wave:$WAVE
# ...

# 2) Assign in parallel (orchestrator-side)
i=0
for t in $(qp list --tag wave:$WAVE --state ready --json | jq -r '.[].display_id'); do
  i=$((i+1))
  qp assign $t --to "wave-$WAVE-agent-$i"
done

# 3) Dispatch subagents — each agent runs:
qp claim  $t --as "wave-$WAVE-agent-$i"
# ... do work ...
qp complete $t --as "wave-$WAVE-agent-$i" --decision "result summary"

# 4) Wait for the wave to drain (running → done/pending). --cohort-done blocks
#    until total>0 && non-terminal==0 for the tagged cohort; an empty cohort
#    (typo'd tag) is a distinct error (exit 4), never a silent instant success.
qp wait --tag wave:$WAVE --cohort-done --interval-ms 1000 --timeout-secs 1800

# 5) Adversarial pass — MANDATORY, before committing the wave (see top of file).
#    Spawn a read-only adversarial agent over the wave's diff range. For each
#    surviving finding, file a task:
#    qp add "fix: <finding>" --depends-on <reviewed-QP-N> --tag wave:$WAVE --tag kind:critique
#    Verify the adversarial agent's claims yourself before acting -- it can be wrong
#    too; today one "eliminates it at zero cost" claim held, another's supporting
#    detail did not, though its recommended fix was still right.

# 6) Promote-and-loop: critiques that exist are already tasks; if any are ready, re-enter step 2.
while [ -n "$(qp list --tag wave:$WAVE --tag kind:critique --state ready --json | jq -r '.[].display_id')" ]; do
  for t in $(qp list --tag wave:$WAVE --tag kind:critique --state ready --json | jq -r '.[].display_id'); do
    qp assign $t --to "wave-$WAVE-fix-$t"
  done
  # dispatch + wait
  qp wait --tag wave:$WAVE --cohort-done --interval-ms 1000 --timeout-secs 600
done

# 7) Reclaim any orphans (e.g. subagent crashed without completing)
for t in $(qp wave --json | jq -r '.running[].display_id'); do
  qp reclaim $t --reason "post-wave cleanup"
done
```

## Observability during a run

- `qp watch` in a side terminal: live event stream.
- `qp wave` (one-shot): grouped `{ready, assigned, running, pending}`.
- `qp timeline QP-<n>`: everything that happened to one task.
- `qp decisions`: skim autonomous decisions after the wave.
- `qp list --tag wave:$WAVE`: everything in this wave.

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

    qp block QP-3 --as wave-7:agent-a --new "DB schema migration needed for QP-3"

This is shorthand for:

    qp add "DB schema migration needed for QP-3" --tag kind:blocker  # QP-9
    qp depends QP-3 --on QP-9 --as wave-7:agent-a
    qp abandon QP-3 --as wave-7:agent-a   # demoted to pending due to new dep

`kind:blocker` is this skill's convention, not a substrate rule — it is only the default of `qp block --tag`. Another orchestration pattern passes its own (`--tag kind:review`, repeatable; supplying any replaces the default). Nothing in the binary reads the tag back: `qp wave` classifies a task as blocked from its unresolved dep edges alone, so the tag is purely a `qp list --tag` filter handle.

The orchestrator sees QP-9 appear in `qp wave` under `ready`, dispatches an agent to resolve it. When QP-9 completes, `refresh_ready` automatically thaws QP-3 back to `ready`, and the orchestrator re-dispatches it.

Exploratory planners use the same primitive *without* `qp block` — they just call `qp depends parent --on child` to push planning work down the DAG.

### Harness attribution

`agent_id` is a free-form string. The convention is `<harness>:<instance>`:

- `claude-code:main`, `claude-code:wt-wave-7-agent-a` — Claude Code sessions
- `cli:nando` — a human operator running `qp` directly
- `cron:nightly-gc` — automated maintenance

Pair this with a `harness:<name>` tag on any task the harness creates, so `qp list --tag harness:claude-code` filters cleanly. The substrate doesn't enforce this — it's an orchestrator convention.

### Editing tasks

Task title, tier, and description are editable after creation:

    qp edit QP-3 --title "fetch staging credentials" --description "see Linear INGEST-42"

Each call emits one `edit` event capturing the before/after of every changed field. No-ops are silently skipped (no event). State, display_id, and created_at are not editable. Edits are allowed in any non-terminal state — including `running` — for scope-refinement mid-flight.

`--as <agent>` is optional and only used for attribution in the `edit` event payload.
