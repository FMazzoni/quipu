# Classifying a claim

Work claim by claim. For each statement, decide which kind it is:

**Behavioural claims** — what the system does. `qp wait --cohort-done exits 4
on an empty cohort`. These are verifiable and must be traced to one of:
- a test in `tests/` that asserts it, or
- a command you can actually run.

If a behavioural claim has neither, that is itself a finding: either the claim
is wrong, or a test is missing.

**Structural claims** — how the code is arranged. `refresh_ready is the only
function that computes readiness`. Verify by reading the code. These rot
fastest, because refactors move code without changing behaviour.

**Rationale** — why a decision was made. Not verifiable from code. Check it
against known knowledge sources — `qp decisions`, a knowledge vault, or the
surrounding context — not against the source. Leave it alone if it merely sounds
outdated.

**When `$QUIPU_VAULT` is unset or the path does not exist** — the vault is
external and per-machine, so this is the common case, not an error. Check
rationale against `qp decisions` alone, mark every claim you could not reach
`unverifiable`, and name the vault as the reason. Never edit or delete a
rationale claim you could not check — an unreachable source is not evidence the
claim is wrong. Keep verifying behavioural and structural claims as normal.
