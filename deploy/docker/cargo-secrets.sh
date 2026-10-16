#!/bin/sh
# Runs a command (cargo) with the optional build secrets in effect, which every
# service's Dockerfile mounts and none keeps in a layer:
#   github_token -- only if a git dependency is private (wyvrn-cloud/didcomm is public):
#                   --secret id=github_token,env=GITHUB_TOKEN
#   ca_bundle    -- extra CA certificates, for building behind a TLS-intercepting proxy:
#                   --secret id=ca_bundle,src=/path/to/ca.pem
set -e
if [ -s /run/secrets/ca_bundle ]; then
    cat /etc/ssl/certs/ca-certificates.crt /run/secrets/ca_bundle > /tmp/ca.pem
    export SSL_CERT_FILE=/tmp/ca.pem GIT_SSL_CAINFO=/tmp/ca.pem CARGO_HTTP_CAINFO=/tmp/ca.pem
fi
if [ -s /run/secrets/github_token ]; then
    export GIT_CONFIG_COUNT=1 \
        GIT_CONFIG_KEY_0="url.https://x-access-token:$(cat /run/secrets/github_token)@github.com/.insteadOf" \
        GIT_CONFIG_VALUE_0="https://github.com/"
fi
exec "$@"
