Each `skills/<name>/` is installed as `qp-<name>`, symlinked by default.

The only command in the tree that touches neither the database nor the state
machine. It exists because the orchestration patterns deliberately live outside
the binary — see the crate docs — and something still has to get them onto disk
where an agent harness will look.

Symlinking is the default, and that is a decision rather than an implementation
detail: a symlinked skill tracks the checkout, so editing a skill in the repo
takes effect immediately and a `git pull` updates every installed copy. `--copy`
exists for the case where the checkout is not going to stay put.

The `qp-` prefix is not cosmetic. Installation removes the destination before
writing it, so the prefix is what keeps a skill named `wave` from clobbering an
unrelated `wave` skill the user already had. `guard_destructive_target` enforces
that invariant at the point of deletion — it refuses to remove any path that
lacks the prefix or is suspiciously shallow, on the theory that a relative target
resolved against an unexpected cwd is exactly how a recursive delete finds
somewhere it should not be. The check is redundant with how the path is
constructed today, and stays because the cost of being wrong here is not bounded
by the database.

`HOME` being unset is a hard error rather than a fallback to a default path
(`install_skills_fails_hard_when_home_unset_and_no_target`). Guessing a
destination is how files end up somewhere the user will never find them.
