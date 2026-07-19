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
# --html-after-content lands before </body> with the DOM parsed, so the script
# needs no DOMContentLoaded wrapper. The path is resolved against the package
# root, so an absolute path is used to survive --manifest-path invocations.
docs:
    RUSTDOCFLAGS="--html-after-content $(pwd)/docs/assets/verify-button.html" \
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
