# Built from the root of the repository, which is the workspace this service is a
# member of (docker-compose.yml does: `context: .`, `dockerfile: services/announce/Dockerfile`).
#
# Optional build secrets:
#   github_token -- only if a git dependency is private (wyvrn-cloud/didcomm is public):
#                   --secret id=github_token,env=GITHUB_TOKEN
#   ca_bundle    -- extra CA certificates, for building behind a TLS-intercepting proxy:
#                   --secret id=ca_bundle,src=/path/to/ca.pem
# Neither ends up in any image layer.
FROM rust:1-slim-bookworm AS builder

# git: cargo fetches the didcomm git dependency with the git CLI (.cargo/config.toml).
# cmake/gcc/perl: aws-lc-rs (rustls' crypto provider) builds a C library; gcc also
# builds the bundled SQLite.
RUN apt-get update && apt-get install -y --no-install-recommends git cmake gcc perl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
# The whole workspace: cargo needs every member's manifest to read the lock file,
# and this service's own crates are among them.
COPY crates crates
COPY services services

# aws-lc-rs' cmake build ignores cargo's -j; cap it so constrained builders don't OOM.
ENV CMAKE_BUILD_PARALLEL_LEVEL=2
RUN --mount=type=secret,id=github_token,required=false \
    --mount=type=secret,id=ca_bundle,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    set -e; \
    if [ -s /run/secrets/ca_bundle ]; then \
        cat /etc/ssl/certs/ca-certificates.crt /run/secrets/ca_bundle > /tmp/ca.pem; \
        export SSL_CERT_FILE=/tmp/ca.pem GIT_SSL_CAINFO=/tmp/ca.pem CARGO_HTTP_CAINFO=/tmp/ca.pem; \
    fi; \
    if [ -s /run/secrets/github_token ]; then \
        export GIT_CONFIG_COUNT=1 \
            GIT_CONFIG_KEY_0="url.https://x-access-token:$(cat /run/secrets/github_token)@github.com/.insteadOf" \
            GIT_CONFIG_VALUE_0="https://github.com/"; \
    fi; \
    cargo build --release --locked -j 2 -p wyvrn-announce

FROM debian:bookworm-slim
# ca-certificates: did:web resolution, and sending to subscribers' HTTPS mediators.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /app announce
WORKDIR /app
COPY --from=builder /build/target/release/wyvrn-announce /usr/local/bin/wyvrn-announce
COPY services/announce/config config
# The identity (and with it the DID), every post, and who is subscribed.
RUN mkdir -p data && chown announce data
VOLUME /app/data
USER announce
EXPOSE 8080
ENTRYPOINT ["wyvrn-announce"]
