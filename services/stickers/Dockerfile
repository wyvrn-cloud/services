# Built from the folder wyvrn-services is in, which also holds wyvrn-sign-in: the
# community's path dependency, which every member of the workspace needs to read
# the lock file (docker-compose.yml does: `context: ..`,
# `dockerfile: wyvrn-services/services/stickers/Dockerfile`; Dockerfile.dockerignore
# beside this says what of that folder is sent).
#
# Optional build secrets:
#   github_token -- only if a git dependency is private (wyvrn-cloud/didcomm is public):
#                   --secret id=github_token,env=GITHUB_TOKEN
#   ca_bundle    -- extra CA certificates, for building behind a TLS-intercepting proxy:
#                   --secret id=ca_bundle,src=/path/to/ca.pem
# Neither ends up in any image layer.
# ---------------------------------------------------------------------------
# The shared base: the same in every service's Dockerfile, word for word, so that
# Docker builds it once and every other service's build finds it already made
# (scripts/check-dockerfiles.sh keeps them the same). It compiles what every
# service has in common -- the dependencies of crates/wyvrn-service, wyvrn-didcomm
# among them -- as an image layer, which a change to the code leaves alone: it is
# made again only when a Cargo.toml or Cargo.lock changes. What a service needs
# beyond that, and its own code, is compiled in its own stage below.
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

FROM chef AS base
COPY wyvrn-sign-in/Cargo.toml wyvrn-sign-in/Cargo.lock /build/wyvrn-sign-in/
COPY wyvrn-sign-in/src /build/wyvrn-sign-in/src
COPY --from=planner /build/wyvrn-services/recipe.json recipe.json
COPY wyvrn-services/.cargo .cargo
COPY wyvrn-services/deploy/docker/cargo-secrets.sh /usr/local/bin/cargo-secrets
RUN --mount=type=secret,id=github_token,required=false \
    --mount=type=secret,id=ca_bundle,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo-secrets cargo chef cook --release --locked -j 2 -p wyvrn-service --recipe-path recipe.json
# --------------------------- end of the shared base ---------------------------

# This service's own dependencies, beyond the shared base: an image layer of its
# own, made again only when a manifest or the lock file changes, so that a change
# to the code compiles the code alone.
FROM base AS deps
RUN --mount=type=secret,id=github_token,required=false \
    --mount=type=secret,id=ca_bundle,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo-secrets cargo chef cook --release --locked -j 2 -p wyvrn-stickers --recipe-path recipe.json

FROM deps AS builder
COPY wyvrn-services/Cargo.toml wyvrn-services/Cargo.lock ./
COPY wyvrn-services/.cargo .cargo
# The whole workspace: cargo needs every member's manifest to read the lock file,
# and this service's own crates are among them.
COPY wyvrn-services/crates crates
COPY wyvrn-services/services services
# Its code, on top of everything it depends on.
RUN --mount=type=secret,id=github_token,required=false \
    --mount=type=secret,id=ca_bundle,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo-secrets cargo build --release --locked -j 2 -p wyvrn-stickers \
    && cp target/release/wyvrn-stickers /usr/local/bin/wyvrn-stickers

FROM debian:bookworm-slim
# ca-certificates: resolving the DIDs of whoever writes to the service, and answering them.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /app stickers
WORKDIR /app
COPY --from=builder /usr/local/bin/wyvrn-stickers /usr/local/bin/wyvrn-stickers
COPY wyvrn-services/services/stickers/config config
# The service's own identity, and every pack it hosts.
RUN mkdir -p data && chown stickers data
VOLUME /app/data
USER stickers
EXPOSE 8080
ENTRYPOINT ["wyvrn-stickers"]
