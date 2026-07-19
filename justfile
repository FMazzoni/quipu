# build and install qp to ~/.cargo/bin
install:
    cargo install --path .

# release build (no install)
build:
    cargo build --release

# run the full test suite, one target at a time
#
# Deliberately NOT a bare `cargo test`. The qp-implement skill forbids agents
# from running one — concurrent rustc graphs OOM the machine — so a sanctioned
# `just` recipe ending in one puts the gate in conflict with the rule, and
# agents resolve that by skipping the gate (QP-167). Looping the targets runs
# exactly the same tests with one rustc live at a time.
#
# `ls tests` rather than a hardcoded list, so a new integration target is gated
# the day it is added. `--bins`, not `--lib`: quipu has no src/lib.rs, so the
# unit tests compile into the `qp` binary and `--lib` matches no target, prints
# nothing, and silently drops all of them. Do not "simplify" this to `--tests`,
# which fans the targets back out in parallel and gives up the whole point.
#
# The `[doc]` attribute rather than the leading comment: `just --list` shows only
# the *last* comment line before a recipe, so a multi-paragraph rationale like
# this one turns the listing into a fragment. `[doc]` decouples the two.
[doc("run the full test suite, one target at a time")]
test:
    #!/usr/bin/env bash
    set -euo pipefail
    for t in $(ls tests | sed 's/\.rs$//'); do
        cargo test --test "$t"
    done
    cargo test --bins

# apply rustfmt
fmt:
    cargo fmt --all

# rustfmt check only (no writes)
fmt-check:
    cargo fmt --all -- --check

# rustdoc warning gate (hard fail)
#
# Use `cargo rustdoc -- -D warnings`, NOT `RUSTDOCFLAGS="-D warnings" cargo doc`.
# RUSTDOCFLAGS *replaces* the `[build] rustdocflags` in .cargo/config.toml, so the
# naive env-var form silently drops the --html-after-content injection: the docs
# still build, the gate still passes, and the copy-fix button + item-table styling
# vanish with no error (the bug fixed in a79558c). Args after `--` are *appended*
# to the config value instead, so both survive. Verified against
# target/doc/qp/index.html: this form keeps `qp-verify` and `fit-content(28%)`,
# the env-var form loses them.
#
# `cargo rustdoc` documents the single target (bin `qp`) and takes no --no-deps.
# If a lib target is ever added it errors on ambiguity rather than silently gating
# only half the crate — fix it then by gating each target explicitly.
doc-check:
    cargo rustdoc -- -D warnings

# the gate: formatting + clippy (denied) + rustdoc (denied) + tests
#
# `&& test` is a post-dependency: it runs after this recipe's body, so the cheap
# static gates still fail first. The tests come from the `test` recipe rather
# than an inline `cargo test` so there is exactly one definition of how this
# repo runs its suite, and it is the per-target sequential one an agent is
# allowed to run (QP-167).
[doc("formatting + rustdoc + clippy gates, then the full suite")]
lint: fmt-check doc-check && test
    cargo clippy --all-targets -- -D warnings

# browsable code docs, with the "copy fix command" button injected
# The --html-after-content flag lives in .cargo/config.toml so that a bare
# `cargo doc` injects it too — otherwise that silently reverts the styling.
docs:
    cargo doc --no-deps
    @echo "open: file://$(pwd)/target/doc/qp/index.html"

# dump every rendered module page as text, for the verify-docs sweep
#
# The denominator is `index.html` only: the crate front page plus one page per
# module. `find target/doc -name '*.html'` would also sweep target/doc/src/, the
# syntax-highlighted source mirror, which is not documentation; dropping the
# -name filter under target/doc/qp still pulls in every per-item page and
# quintuples the count.
#
# w3m, not lynx/pandoc/html2text — those are not installed here. It halves the
# bytes versus raw HTML, which is what makes the sweep fit in an agent's context.
docs-dump: docs
    @find target/doc/qp -name 'index.html' | sort | while read -r p; do \
        printf '\n===== %s =====\n' "$p"; \
        w3m -dump -cols 100 "$p"; \
    done

# stripped-binary size + cold start (verifies leanness budget)
# The RSS probe needs GNU time (-v); it degrades to a skip elsewhere (macOS/BSD).
check-lean:
    cargo build --release
    @strip target/release/qp
    @du -h target/release/qp
    @if /usr/bin/time -v true >/dev/null 2>&1; then \
        /usr/bin/time -v target/release/qp --version 2>&1 | grep "Maximum resident"; \
    else \
        echo "(RSS check skipped: GNU /usr/bin/time -v unavailable on this platform)"; \
    fi
