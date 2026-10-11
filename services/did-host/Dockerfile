# Built from the root of the repository, which is the workspace this service is a
# member of (docker-compose.yml does: `context: .`, `dockerfile: services/did-host/Dockerfile`).
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

# The build's output is kept between builds (a cache mount, shared by every
# service here, since they are one workspace): a change rebuilds what it touches,
# not every dependency again. A cache mount is not part of the image, so the
# binary is copied out of it in the same step.
# aws-lc-rs' cmake build ignores cargo's -j; cap it so constrained builders don't OOM.
ENV CMAKE_BUILD_PARALLEL_LEVEL=2
RUN --mount=type=secret,id=github_token,required=false \
    --mount=type=secret,id=ca_bundle,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target,id=wyvrn-services-target,sharing=locked \
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
    cargo build --release --locked -j 2 -p wyvrn-did-host; \
    cp target/release/wyvrn-did-host /usr/local/bin/wyvrn-did-host

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
