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

# the gate: formatting + clippy (denied) + tests
lint: fmt-check
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
