# Built from the root of the repository, which is the workspace this service is a
# member of (docker-compose.yml does: `context: .`, `dockerfile: services/did-host/Dockerfile`).
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
WORKDIR /build
# aws-lc-rs' cmake build ignores cargo's -j; cap it so constrained builders don't OOM.
ENV CMAKE_BUILD_PARALLEL_LEVEL=2

# What the workspace's manifests say, and nothing of its code: the recipe changes
# only when they do, and so does everything built from it.
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates
COPY services services
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS base
COPY --from=planner /build/recipe.json recipe.json
COPY .cargo .cargo
COPY deploy/docker/cargo-secrets.sh /usr/local/bin/cargo-secrets
RUN --mount=type=secret,id=github_token,required=false \
    --mount=type=secret,id=ca_bundle,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo-secrets cargo chef cook --release --locked -j 2 -p wyvrn-service --recipe-path recipe.json
# --------------------------- end of the shared base ---------------------------

FROM base AS builder
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
# The whole workspace: cargo needs every member's manifest to read the lock file,
# and this service's own crates are among them.
COPY crates crates
COPY services services
# On top of the shared base: only what this service needs beyond it, and its code.
RUN --mount=type=secret,id=github_token,required=false \
    --mount=type=secret,id=ca_bundle,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo-secrets cargo build --release --locked -j 2 -p wyvrn-did-host \
    && cp target/release/wyvrn-did-host /usr/local/bin/wyvrn-did-host

FROM debian:bookworm-slim
# ca-certificates: resolving the DIDs of whoever writes to the host, and answering them.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /app didhost
WORKDIR /app
COPY --from=builder /usr/local/bin/wyvrn-did-host /usr/local/bin/wyvrn-did-host
COPY services/did-host/config config
# The host's own identity, and every name and DID log it hosts.
RUN mkdir -p data && chown didhost data
VOLUME /app/data
USER didhost
EXPOSE 8080
ENTRYPOINT ["wyvrn-did-host"]
