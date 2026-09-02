# syntax=docker/dockerfile:1

# The deployable is one binary: `crates/iaam-bootstrap` builds `iaam`, which is
# both the server (`iaam serve`) and the local administration CLI (ADR-0003).
# One image therefore covers both roles; which role a container plays is decided
# by the command it is given, not by the image.
#
# The image contains the program and nothing else. No database, no broker
# encryption key, no account map, no token, no bind address. Every one of those
# is supplied at run time by the operator, from outside this repository. A
# default here would be a value baked into a published artefact, which is the
# one thing this project does not allow.

ARG RUST_VERSION=1
ARG DEBIAN_RELEASE=trixie

# --- Build -------------------------------------------------------------------
# `rust:*-slim-*` already carries gcc and libc headers: `rusqlite` is built with
# the bundled SQLite amalgamation and `ring` compiles C, so a compiler is not
# optional. Nothing else is installed.
FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS build

WORKDIR /src

# Only what the release build reads. `tests/` holds fixtures used by test
# targets, which this build does not compile; leaving them out of the context
# keeps files that look like data out of the builder entirely.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# `--locked` fails rather than silently resolving a different dependency graph:
# an image built from a clean checkout must be the checkout it claims to be.
# The binary is stripped here, in the stage that is thrown away.
RUN cargo build --release --locked --package iaam-bootstrap \
    && strip --strip-all target/release/iaam

# --- Runtime -----------------------------------------------------------------
# Debian slim rather than a distroless or Alpine base: the build is glibc and
# dynamically linked, and the administration subcommands are run interactively
# by a human at a console (`iaam broker access add` reads a token from standard
# input). A base without a shell makes that harder to operate without making the
# image meaningfully smaller.
FROM debian:${DEBIAN_RELEASE}-slim AS runtime

# `ca-certificates` is required, not cosmetic: outbound destinations whose trust
# anchor is `Anchors::WebRoots` (MOEX, CBR) verify against the system store.
# Without it those calls fail at the TLS handshake. The T-Invest anchor is
# embedded in the binary and needs nothing from the image.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# A fixed uid/gid, because the operator has to chown the host directory holding
# the database to it, and a number that moves between builds turns that into a
# guess. 10001 is outside the range Debian hands out to system packages.
RUN groupadd --system --gid 10001 iaam \
    && useradd --system --uid 10001 --gid 10001 --home-dir /var/lib/iaam \
       --shell /usr/sbin/nologin iaam \
    && install --directory --owner iaam --group iaam --mode 0700 /var/lib/iaam

COPY --from=build /src/target/release/iaam /usr/local/bin/iaam

# Never root. The database file and the broker key file are readable by whoever
# the process runs as, and root in the container is root on a bind mount.
USER 10001:10001
WORKDIR /var/lib/iaam

# Documentation only: EXPOSE binds nothing. The program's own default is
# 127.0.0.1:8080, which inside a container is reachable only from that
# container, so a published port requires IAAM_LISTEN=0.0.0.0:8080 to be passed
# at run time. That stays an explicit act (see docs/deployment.md).
EXPOSE 8080

# No VOLUME: an anonymous volume created because the operator forgot `--mount`
# would hold the owner's whole journal somewhere nobody named and nobody backs
# up. Failing to start without a mount is better than succeeding into nowhere.
#
# No HEALTHCHECK: it would need a HTTP client in the runtime image for a check
# the orchestrator or the guide can make from outside against /v1/health.
ENTRYPOINT ["/usr/local/bin/iaam"]
CMD ["serve"]
