# A generic container image for `qp serve`.
#
# It names no host, no volume and no port mapping to a real machine: what to
# publish, where the store lives and what fronts the port are the operator's,
# exactly as `docs/process-contract.md` says. What this file fixes is only the
# three things that are properties of the process rather than of a deployment —
# the binary, the store directory, and a foreground entrypoint that a supervisor
# can stop with SIGTERM.
#
# Build:   docker build -t quipu:local .
# Run:     docker run -d --name qp -p 127.0.0.1:8787:8787 \
#              -v qp-store:/var/lib/quipu quipu:local
#
# The publish form matters and is not decoration: `-p 127.0.0.1:8787:8787`
# binds the host side to loopback, so plaintext still crosses only the host's
# loopback. See "Publishing a container port" in `docs/process-contract.md` §8
# for why the bind inside the container is `0.0.0.0` and why that is not the
# arrangement the same section forbids.

# Not `rust-version` from Cargo.toml, and the gap is a real one worth naming:
# that field says 1.85, and the locked dependency graph does not build there —
# `cargo build --locked` on 1.85 refuses outright, because `icu_*`, `time` and
# `idna_adapter` as pinned in Cargo.lock require 1.88. The declared MSRV
# describes the source; this describes what the lockfile actually needs. Raise
# it when the lockfile moves, and do not lower it to match Cargo.toml.
ARG RUST_VERSION=1.88

FROM rust:${RUST_VERSION}-slim-bookworm AS build
# Bounded rather than "all cores": rusqlite's bundled SQLite is a large C
# compile, and an unbounded `cargo build` inside a memory-capped builder is the
# one step of this image that gets OOM-killed.
ARG CARGO_BUILD_JOBS=2
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
WORKDIR /src
# Explicit copies rather than `COPY . .`: `docs/` and `skills/` are not
# incidental, they are `include_str!` targets, and a build that omits either
# fails at compile time rather than shipping something incomplete.
COPY Cargo.toml Cargo.lock README.md ./
COPY src ./src
COPY docs ./docs
COPY skills ./skills
RUN cargo build --release --locked && strip target/release/qp

FROM debian:bookworm-slim
# A non-root uid, fixed rather than allocated, because it has to match the
# ownership of a named volume that outlives any one container.
RUN useradd --system --uid 10001 --user-group --home-dir /var/lib/quipu quipu \
 && mkdir -p /var/lib/quipu \
 && chown 10001:10001 /var/lib/quipu
COPY --from=build /src/target/release/qp /usr/local/bin/qp

# The store directory, and the working directory the server resolves its store
# from when no `--project` is named. Declared as a VOLUME so that a store can
# never end up written into the image's own writable layer, where it would be
# discarded with the container and would take the event log with it.
WORKDIR /var/lib/quipu
VOLUME ["/var/lib/quipu"]

# SQLite locking is only correct on a local filesystem. Whatever is mounted
# here must be local disk — a bind mount from a network share passes every
# smoke test and corrupts under contention. `docker-smoke.sh` asserts it.

USER quipu
EXPOSE 8787

# Foreground, PID 1, no shell wrapper: the process contract requires that
# SIGTERM reach `qp serve` itself, and an `ENTRYPOINT` in shell form would put
# `/bin/sh` in front of it and swallow the signal. `--bind 0.0.0.0` is the
# container's own network namespace, not the host's interfaces.
ENTRYPOINT ["qp", "serve"]
CMD ["--bind", "0.0.0.0:8787"]
