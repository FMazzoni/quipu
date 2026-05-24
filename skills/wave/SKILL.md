---
name: qp-wave
description: Execute a wave with qp. Plan tasks, assign in parallel, dispatch subagents, critique, loop until clean.
---

## When to use
Independent work units that can be parallelized, with a critique step.

## Conventions

- **Wave id:** every task in a wave is tagged `wave:<id>`.
- **Critique tasks:** tagged `kind:critique`, `--depends-on <reviewed-task>`.
- **Non-blocking findings:** add `blocking:false` tag if the critique should not gate progress.
- **Variants (branch-and-evaluate):** use `qp relation add <T> variant-of <root>`. After picking a winner, `qp cancel` losers.
- **Decisions:** agents log autonomous decisions via `qp log T<n> decision "..." --as <agent> --auto`.

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

# 4) Wait for the wave to drain (running → done/blocked)
qp wait --tag wave:$WAVE --state running --empty --interval-ms 1000 --timeout-secs 1800

# 5) Critique pass (spawn a critic; for each issue):
#    qp add "fix: <finding>" --depends-on <reviewed-T> --tag wave:$WAVE --tag kind:critique

# 6) Promote-and-loop: critiques that exist are already tasks; if any are ready, re-enter step 2.
while [ -n "$(qp list --tag wave:$WAVE --tag kind:critique --state ready --json | jq -r '.[].display_id')" ]; do
  for t in $(qp list --tag wave:$WAVE --tag kind:critique --state ready --json | jq -r '.[].display_id'); do
    qp assign $t --to "wave-$WAVE-fix-$t"
  done
  # dispatch + wait
  qp wait --tag wave:$WAVE --state running --empty --interval-ms 1000 --timeout-secs 600
done

# 7) Reclaim any orphans (e.g. subagent crashed without completing)
for t in $(qp wave --json | jq -r '.running[].display_id'); do
  qp reclaim $t --reason "post-wave cleanup"
done
```

## Observability during a run

- `qp watch` in a side terminal: live event stream.
- `qp wave` (one-shot): grouped `{ready, running, blocked}`.
- `qp timeline T<n>`: everything that happened to one task.
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
WINNER=T<x>; for L in $(qp relation list $ROOT --json | jq -r ".incoming[].from" | grep -v ^$WINNER$); do
  qp cancel $L --reason "superseded by $WINNER"
done
qp relation add $WINNER supersedes $ROOT
```
