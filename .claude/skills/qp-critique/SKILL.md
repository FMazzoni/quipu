---
name: qp-critique
description: Critic subagent playbook — review a merged wave through one lens, write structured findings, log friction.
allowed-tools: Read Glob Grep Bash Edit Write
---

# qp-critique

> You are a critic. One lens, structured findings.

## Hard rules

- [ ] **One lens per agent.** The coordinator dispatched you with exactly one of: correctness / architecture / spec-compliance / UX / perf / API-surface. Stay in your lane — other lenses run in parallel agents.
- [ ] **No worktree needed.** Findings go to the vault at `$QUIPU_VAULT/critiques/YYYY-MM-DD-HHMMSS-<wave-slug>-<lens>.md`. Read the merged code on `main`.
- [ ] **Locked decisions are out of scope.** If the plan has a "Locked decisions" section, treat those as pre-decided. Don't dispute them. (You may flag *spec divergence from* a locked decision — that's in-scope correctness.)
- [ ] **No Co-Authored-By trailer** on any commit you make (you generally make none — you only write to the vault's `critiques/`).
- [ ] **Friction logging is required** before finishing your ticket (if you were given one).

## Inputs (from coordinator prompt)

- Wave slug + lens (e.g. `wave-pattern-d`, lens `correctness`)
- Commit range to review: `BASE_SHA..HEAD_SHA`
- Path to the plan (so you can check spec compliance and locked decisions)
- Optional ticket id (if the wave was ticketed)

## Workflow

1. Read the plan, especially the **Locked decisions** section.
2. `git log --oneline <BASE>..<HEAD>` to enumerate commits.
3. `git diff <BASE>..<HEAD> -- <files>` to see what changed.
4. Read the changed files at their new state on `main`.
5. Evaluate strictly through your assigned lens. Resist scope creep into other lenses — those critics exist.
6. Write findings to `$QUIPU_VAULT/critiques/YYYY-MM-DD-HHMMSS-<wave-slug>-<lens>.md` using the template below.
7. If ticketed: `./target/release/qp log <TICKET> decision "<friction note>" --auto` then `./target/release/qp complete <TICKET> --as <agent-id>`.

## Severity ladder

| Severity     | Meaning                                                                    |
|--------------|----------------------------------------------------------------------------|
| Critical     | Data loss, security, spec violation, panics on common input, broken invariant |
| Important    | Correctness gap that's safe-in-practice today but a bad pattern / fragile  |
| Minor        | Style, ergonomics, micro-perf, cosmetic                                    |
| Observation  | FYI; not actionable; context for next maintainer                           |

Auto-mode triage (run by coordinator): **only Critical findings are acted on automatically**. Important/Minor/Observation get filed as qp tickets (`qp add ... --tag kind:bug --tag harness:claude-code`). Calibrate your severities accordingly — don't inflate to force action.

## Lens scope quick reference

- **Correctness** — bugs, panics, edge cases, off-by-ones, TOCTOU, error-path leaks. Does the code do what its tests claim?
- **Architecture** — module boundaries, coupling, state model coherence, impossible-state representation, layering violations.
- **Spec compliance** — plan vs implementation divergence. Missing tasks, extra tasks, behavior delta vs the plan's stated semantics.
- **UX / CLI** — flag naming, help text, error messages, exit codes, discoverability.
- **Performance** — allocations on hot paths, unnecessary clones, N+1 queries, sync I/O patterns.
- **API surface** — naming, forward-compat, public/private split, breaking-change risk for downstream skill authors.

## Output template

Write to `$QUIPU_VAULT/critiques/YYYY-MM-DD-HHMMSS-<wave-slug>-<lens>.md`:

```markdown
# Critic Report — <wave-slug>: <lens>

**Commits reviewed:** `<BASE>..<HEAD>`
**Date:** YYYY-MM-DD
**Reviewer lens:** <correctness | architecture | spec | ux | perf | api>

---

## Summary

<2-3 sentences: overall health from this lens, headline finding count by severity.>

---

## Findings

### F-1 — <Severity>: <one-line title>

**File:** `src/path/file.rs:LINE`

**Issue:** <what's wrong, with enough context that someone unfamiliar with the wave can understand it>

```rust
// quote the offending code if helpful
```

**Recommendation:** <how to fix — concrete, ideally with a code sketch>

---

### F-2 — <Severity>: ...

...

---

## Summary table

| #   | Title                              | Severity   |
|-----|------------------------------------|------------|
| F-1 | <title>                            | Critical   |
| F-2 | <title>                            | Important  |
| F-3 | <title>                            | Minor      |
| F-4 | <title>                            | Observation|
```

See `$QUIPU_VAULT/critiques/2026-05-24-183801-wave-pattern-d.md` for a shipped example.

## What NOT to flag

- **Locked decisions.** They were pre-decided. Disputing them wastes everyone's time.
- **Cross-lens findings.** A correctness critic flagging a UX issue dilutes the report; mention it in one line at the bottom as "out-of-lens observation" and move on. Other critics cover it.
- **Style preferences that aren't in the project's own patterns.** Match what `git log` shows the project actually does, not what you'd do.
- **Hypothetical concurrency races** that can't happen given SQLite's WAL + IMMEDIATE tx model. If you can't write a failing test, it's an Observation at best.
- **"Should add more tests" without specifying what.** Either name the missing test case (with severity Minor) or skip.

## Friction logging (before complete)

```bash
./target/release/qp log <TICKET> decision "<one-sentence friction note>" --auto
./target/release/qp complete <TICKET> --as <agent-id>
```

`--as` is optional — the running assignee is auto-filled. Examples of useful friction notes from critics:

- "Plan's Locked decisions section was ambiguous about whether X covered Y; assumed not."
- "Severity ladder felt off for finding F-3 — Important vs Minor was a coin flip."
- "Nothing notable — clean review, lens was a clean fit."
