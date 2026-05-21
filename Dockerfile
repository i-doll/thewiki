# syntax=docker/dockerfile:1.7
#
# thewiki container image.
#
# Multi-stage build producing a small, non-root, multi-arch runtime image.
# The runtime base is `gcr.io/distroless/cc-debian12:nonroot`, which gives us
# glibc + libgcc (so we can dynamically link the Rust binary without lugging in
# a full distro), a non-root uid (65532:65532), and no shell / no package
# manager in the final image.
#
# Two builder stages run natively on the *build* host (`$BUILDPLATFORM`):
#   1. `rust-build`  — cross-compiles the workspace for `$TARGETPLATFORM`
#                      using cargo-chef for dependency caching.
#   2. `web-build`   — builds the React frontend with pnpm.
#
# The Rust binary does not yet embed `web/dist/` (that lands in #16 via
# `rust-embed`). We still produce the frontend bundle here and copy it into
# the final image so the moment #16 merges, deploys pick up the static assets
# without a Dockerfile churn. Until then, `web/dist/` is dead weight at
# `/srv/web/dist/` inside the image — kilobytes, not megabytes, and it keeps
# release infrastructure stable across the embed switch.
#
# Healthcheck: there's intentionally no `HEALTHCHECK` instruction here.
# Distroless ships no `wget`/`curl`/`sh`, and the `thewiki` binary does not
# yet have a `healthcheck` subcommand that would let us probe `/healthz`
# in-process. That subcommand is a small follow-up; once it lands, add:
#     HEALTHCHECK --interval=30s --timeout=3s CMD ["thewiki", "healthcheck"]
# Orchestrators (Kubernetes, Nomad, Compose v3) can already probe
# `GET /healthz` directly today, so this is only missing for plain
# `docker run`. Tracked alongside #7.

ARG RUST_VERSION=1.92
ARG NODE_VERSION=24
ARG DEBIAN_RELEASE=bookworm

###############################################################################
# Stage: chef — shared base for the Rust build pipeline.                      #
###############################################################################
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-${DEBIAN_RELEASE} AS chef
WORKDIR /src
# `cargo-chef` lets us cache the dependency graph independently of the source
# tree, so iterating on `crates/api/src/*.rs` doesn't blow away the dep build.
RUN cargo install cargo-chef --locked --version 0.1.77

###############################################################################
# Stage: planner — computes the dependency recipe (no source builds).         #
###############################################################################
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

###############################################################################
# Stage: rust-build — cross-compiles the workspace for $TARGETPLATFORM.       #
###############################################################################
FROM chef AS rust-build
ARG TARGETPLATFORM
ARG BUILDPLATFORM

# Map Docker's $TARGETPLATFORM to a Rust target triple and install the
# corresponding cross toolchain (linker + target libc). We dynamically link
# against glibc to keep the binary small; the runtime image
# (`distroless/cc-debian12`) ships a compatible glibc.
RUN set -eux; \
    case "$TARGETPLATFORM" in \
        "linux/amd64") \
            RUST_TARGET="x86_64-unknown-linux-gnu"; \
            # `gcc-x86-64-linux-gnu` and `libc6-dev-amd64-cross` are only
            # strictly needed when $BUILDPLATFORM != amd64. They're cheap
            # to pull in and keep the logic symmetric.
            APT_PACKAGES="gcc-x86-64-linux-gnu libc6-dev-amd64-cross"; \
            ;; \
        "linux/arm64") \
            RUST_TARGET="aarch64-unknown-linux-gnu"; \
            # `libc6-dev-arm64-cross` ships Scrt1.o / crti.o for the target,
            # without which the final link step fails with `cannot find crti.o`.
            APT_PACKAGES="gcc-aarch64-linux-gnu libc6-dev-arm64-cross"; \
            ;; \
        *) echo "unsupported TARGETPLATFORM: $TARGETPLATFORM" >&2; exit 1 ;; \
    esac; \
    echo "$RUST_TARGET" > /tmp/rust_target; \
    apt-get update; \
    apt-get install -y --no-install-recommends $APT_PACKAGES; \
    rm -rf /var/lib/apt/lists/*

# Linker config for cross-builds. cargo reads CARGO_TARGET_<TRIPLE>_LINKER.
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc

# Strip symbols at link time for a leaner binary. `lto = "thin"` and
# `codegen-units = 1` already live in `[profile.release]` in Cargo.toml.
ENV RUSTFLAGS="-C strip=symbols"

# Bring rust-toolchain.toml into place BEFORE adding the cross target.
# rustup reads `rust-toolchain.toml` on every `cargo` / `rustup` invocation
# from within the workspace; if we add the target against the base image's
# default toolchain and then rust-toolchain.toml asks for a different
# channel/profile on the first `cargo` call, the target gets dropped.
# Copying just this one file (instead of the full source) keeps the
# dependency-cache layers below intact.
COPY rust-toolchain.toml ./rust-toolchain.toml

# Add the cross target against the toolchain pinned by rust-toolchain.toml.
RUN RUST_TARGET="$(cat /tmp/rust_target)"; \
    rustup target add "$RUST_TARGET"

# Build the dependency graph from the chef recipe first. `cargo chef cook`
# materializes a workspace skeleton from recipe.json and compiles all
# third-party crates without touching the real source — so this layer is
# reused as long as Cargo.toml / Cargo.lock don't change.
COPY --from=planner /src/recipe.json recipe.json
RUN RUST_TARGET="$(cat /tmp/rust_target)"; \
    cargo chef cook --release --recipe-path recipe.json --target "$RUST_TARGET" -p thewiki-api

# Now bring in the real source and build the binary. This is the only layer
# that rebuilds on source changes.
COPY . .
RUN set -eux; \
    RUST_TARGET="$(cat /tmp/rust_target)"; \
    cargo build --release --target "$RUST_TARGET" -p thewiki-api; \
    mkdir -p /out; \
    cp "target/$RUST_TARGET/release/thewiki" /out/thewiki

###############################################################################
# Stage: web-build — builds the React frontend with pnpm.                     #
###############################################################################
FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-${DEBIAN_RELEASE}-slim AS web-build
WORKDIR /web

# pnpm 10 to match the repo's `engines.pnpm` and the lockfile format.
RUN corepack enable && corepack prepare pnpm@10 --activate

# Install with the lockfile pinned, then build. pnpm fetches into its content-
# addressed store; we don't worry about pruning here because this stage is
# discarded after the COPY in the final stage.
COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile

COPY web/ ./
RUN pnpm build

###############################################################################
# Stage: runtime — distroless, non-root, minimal.                             #
###############################################################################
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

# OCI image labels. `source`, `revision`, and `version` are overridden by the
# GHCR publish workflow via `--label` so they reflect the actual git ref;
# the values baked in here are sensible defaults for local builds.
LABEL org.opencontainers.image.title="thewiki" \
      org.opencontainers.image.description="A self-hosted single-binary wiki for public reference use" \
      org.opencontainers.image.source="https://github.com/i-doll/thewiki" \
      org.opencontainers.image.url="https://github.com/i-doll/thewiki" \
      org.opencontainers.image.documentation="https://github.com/i-doll/thewiki/blob/main/README.md" \
      org.opencontainers.image.licenses="AGPL-3.0-only" \
      org.opencontainers.image.vendor="thewiki contributors" \
      org.opencontainers.image.version="0.1.0" \
      org.opencontainers.image.revision="" \
      org.opencontainers.image.authors="thewiki contributors"

# Default install layout:
#   /usr/local/bin/thewiki   — the server binary
#   /srv/web/dist/           — built frontend (consumed by #16 once embedding
#                              lands; ignored by the binary until then).
COPY --from=rust-build /out/thewiki /usr/local/bin/thewiki
COPY --from=web-build  /web/dist    /srv/web/dist

# Distroless `:nonroot` already sets USER nonroot (uid 65532). Restating it
# keeps the contract explicit so an operator reading the Dockerfile doesn't
# have to know that detail of the base image.
USER nonroot:nonroot

# `serve` binds to 0.0.0.0:8080 by default (see `crates/api/src/cli.rs`).
EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/thewiki"]
CMD ["serve"]
