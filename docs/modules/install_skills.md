Each `skills/<name>/` is installed as `qp-<name>` — symlinked from a checkout,
written from an embedded snapshot otherwise.

The only command in the tree that touches neither the database nor the state
machine. It exists because the orchestration patterns deliberately live outside
the binary — see the crate docs — and something still has to get them onto disk
where an agent harness will look.

## Resolution order

1. `QP_SKILLS_SRC` if set — `$QP_SKILLS_SRC/skills/`. A value that does not
   resolve to a directory is a hard error, not a fallback: an explicit override
   that quietly installs the snapshot instead is worse than a failure, because
   the dev editing skills in that checkout would see no propagation and no
   complaint.
2. Otherwise `current_exe()`'s parent joined with `../../skills`, **probed for
   existence**. That derivation is shaped for a cargo target directory
   (`<root>/target/release/qp` → `<root>/skills`) and does not survive the
   binary being moved: from `~/.cargo/bin/qp` or `~/.local/bin/qp` it derives
   `~/skills`.
3. Otherwise the copy embedded in the binary.

The `exists()` probe in step 2 is the whole difference between the two modes.
It used to be absent, so a downloaded or `cargo install`ed `qp` resolved to a
`~/skills` that was not there and `install-skills` failed for every user who
had not cloned the repo.

## The two modes

**Live** (a source root resolved) keeps the old behaviour exactly: symlink on
unix, `--copy` to opt out. Symlinking is the default so that the quipu repo
stays the single source of truth — skills evolve with the binary, and
co-shipping them keeps the two in sync without any version negotiation between
a skill and the `qp` that expects it; a link is what preserves that after
install. `--copy` exists for the case where the checkout is not going to stay
put. Output reads `installed (linked)` or `installed (copied)`.

**Snapshot** (nothing on disk) writes `EMBEDDED_SKILLS` out. It is always a
copy — there is no file to point a symlink at — so `--copy` is a no-op here,
and the installed skills are frozen at the version of the binary that wrote
them. Output reads `installed (embedded snapshot)`, which is the point of
naming the mode at all: a user needs to know whether they got a live link that
tracks a checkout or a snapshot that will only change when they upgrade `qp`.

Embedding rather than fetching at install time is what keeps the binary
standalone — no network, no second artifact, no version skew between a release
and the skills it expects — at a cost of about 12 KB against the 5 MB budget.
Per-project copies and packaging the skills as a Claude Code plugin were both
weighed and rejected; the plugin route is the better long-term answer but
deferred to v2, since it costs marketplace publishing or a git-clone-and-install
dance. The long form is the decision note `quipu-skills-shipped-from-repo.md`.

`EMBEDDED_SKILLS` is a hand-written list of `include_str!` calls, which is a
list that can silently fall behind the tree — a skill that grows a
`references/` subdirectory would ship incomplete from every release with
nothing failing. `embedded_set_matches_skills_tree` asserts the two agree, and
`embedded_bodies_match_disk` asserts the contents do. Two `include_str!` calls
beat an `include_dir` dependency for a tree this size; the test is what makes
that trade safe.

## Guards

The `qp-` prefix is not cosmetic. Installation removes the destination before
writing it, so the prefix is what keeps a skill named `wave` from clobbering an
unrelated `wave` skill the user already had. `guard_destructive_target` enforces
that invariant at the point of deletion — it refuses to remove any path that
lacks the prefix or is suspiciously shallow, on the theory that a relative target
resolved against an unexpected cwd is exactly how a recursive delete finds
somewhere it should not be. The check is redundant with how the path is
constructed today, and stays because the cost of being wrong here is not bounded
by the database. Both modes go through it, via `replace_target`.

`HOME` being unset is a hard error rather than a fallback to a default path
(`install_skills_fails_hard_when_home_unset_and_no_target`). Guessing a
destination is how files end up somewhere the user will never find them. The
target is resolved before the source, so that failure fires regardless of which
mode would have run.
