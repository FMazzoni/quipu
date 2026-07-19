# build and install qp to ~/.cargo/bin
install:
    cargo install --path .

# release build (no install)
build:
    cargo build --release

# run the full test suite
test:
    cargo test

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
lint: fmt-check doc-check
    cargo clippy --all-targets -- -D warnings
    cargo test

# browsable code docs, with the "copy fix command" button injected
# The --html-after-content flag lives in .cargo/config.toml so that a bare
# `cargo doc` injects it too — otherwise that silently reverts the styling.
docs:
    cargo doc --no-deps
    @echo "open: file://$(pwd)/target/doc/qp/index.html"

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
