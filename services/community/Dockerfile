# Built from the folder wyvrn-services is in, which also holds wyvrn-sign-in: the
# community's path dependency, which every member of the workspace needs to read
# the lock file (docker-compose.yml does: `context: ..`,
# `dockerfile: wyvrn-services/services/community/Dockerfile`; Dockerfile.dockerignore
# beside this says what of that folder is sent).
#
# Optional build secrets:
#   github_token -- only if a git dependency is private (wyvrn-cloud/didcomm is public):
#                   --secret id=github_token,env=GITHUB_TOKEN
#   ca_bundle    -- extra CA certificates, for building behind a TLS-intercepting proxy:
#                   --secret id=ca_bundle,src=/path/to/ca.pem
# Neither ends up in any image layer.
# ---------------------------------------------------------------------------
# The shared build: the same in every service's Dockerfile, word for word, so that
# Docker builds it once and every other service's build finds it already made
# (scripts/check-dockerfiles.sh keeps them the same). It compiles every service
# at once: their dependencies as one image layer, made again only when a
# Cargo.toml or Cargo.lock changes, and then their code. Each service's image
# takes its own binary from it. (Built one service at a time, each of the nine
# compiled the shared crates and most dependencies again.)
#
# As many jobs as the builder has cores; --build-arg CARGO_JOBS=2 for a small one.
# ---------------------------------------------------------------------------
FROM rust:1-slim-bookworm AS chef

# git: cargo fetches the didcomm git dependency with the git CLI (.cargo/config.toml).
# cmake/gcc/perl: aws-lc-rs (rustls' crypto provider) builds a C library; gcc also
# builds the bundled SQLite.
RUN apt-get update && apt-get install -y --no-install-recommends git cmake gcc perl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /build/wyvrn-services
# aws-lc-rs' cmake build ignores cargo's -j; cap it so constrained builders don't OOM.
ENV CMAKE_BUILD_PARALLEL_LEVEL=2

# What the workspace's manifests say, and nothing of its code: the recipe changes
# only when they do, and so does everything built from it.
FROM chef AS planner
COPY wyvrn-sign-in/Cargo.toml wyvrn-sign-in/Cargo.lock /build/wyvrn-sign-in/
COPY wyvrn-sign-in/src /build/wyvrn-sign-in/src
COPY wyvrn-services/Cargo.toml wyvrn-services/Cargo.lock ./
COPY wyvrn-services/.cargo .cargo
COPY wyvrn-services/crates crates
COPY wyvrn-services/services services
RUN cargo chef prepare --recipe-path recipe.json

# Every service's dependencies.
FROM chef AS deps
ARG CARGO_JOBS
COPY wyvrn-sign-in/Cargo.toml wyvrn-sign-in/Cargo.lock /build/wyvrn-sign-in/
COPY wyvrn-sign-in/src /build/wyvrn-sign-in/src
COPY --from=planner /build/wyvrn-services/recipe.json recipe.json
COPY wyvrn-services/.cargo .cargo
COPY wyvrn-services/deploy/docker/cargo-secrets.sh /usr/local/bin/cargo-secrets
RUN --mount=type=secret,id=github_token,required=false \
    --mount=type=secret,id=ca_bundle,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo-secrets cargo chef cook --release --locked ${CARGO_JOBS:+-j $CARGO_JOBS} -p wyvrn-announce -p wyvrn-cards -p wyvrn-claude -p wyvrn-community -p wyvrn-community-host -p wyvrn-did-host -p wyvrn-files -p wyvrn-relay -p wyvrn-stickers --recipe-path recipe.json

# Every service's code, on top of everything it depends on.
FROM deps AS builder
ARG CARGO_JOBS
COPY wyvrn-services/Cargo.toml wyvrn-services/Cargo.lock ./
COPY wyvrn-services/.cargo .cargo
# The whole workspace: cargo needs every member's manifest to read the lock file.
COPY wyvrn-services/crates crates
COPY wyvrn-services/services services
RUN --mount=type=secret,id=github_token,required=false \
    --mount=type=secret,id=ca_bundle,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo-secrets cargo build --release --locked ${CARGO_JOBS:+-j $CARGO_JOBS} -p wyvrn-announce -p wyvrn-cards -p wyvrn-claude -p wyvrn-community -p wyvrn-community-host -p wyvrn-did-host -p wyvrn-files -p wyvrn-relay -p wyvrn-stickers
# ---------------------------- end of the shared build ----------------------------

FROM debian:bookworm-slim
# ca-certificates: resolving members' DIDs, and sending to them.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /app community
WORKDIR /app
COPY --from=builder /build/wyvrn-services/target/release/wyvrn-community /usr/local/bin/wyvrn-community
COPY wyvrn-services/services/community/config config
# The server's own identity, who is let in, and everything said.
RUN mkdir -p data && chown community data
VOLUME /app/data
USER community
EXPOSE 8080
ENTRYPOINT ["wyvrn-community"]
