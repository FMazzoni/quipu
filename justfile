# build and install qp to ~/.cargo/bin
install:
    cargo install --path .

# release build (no install)
build:
    cargo build --release

# run the full test suite
test:
    cargo test

# stripped-binary size + cold start (verifies leanness budget)
check-lean:
    cargo build --release
    @strip target/release/qp
    @du -h target/release/qp
    @/usr/bin/time -v target/release/qp --version 2>&1 | grep "Maximum resident"
