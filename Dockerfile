# syntax=docker/dockerfile:1
#
# Multi-stage image for Microwave Server.
#
# Builds the three deployable binaries from the Cargo workspace:
#   * microwave-server    — the HTTP daemon
#   * microwave-ctl       — the operational CLI
#   * chef-server-ctl     — compatibility alias built from the same CLI crate
#   * microwave-migrate   — snapshot export/import
#
# Build:
#   docker build -t microwave-server:local .
#   docker build --build-arg RUST_VERSION=1.94 -t microwave-server:1.94 .
#
# The runtime image runs as an unprivileged user and keeps all mutable state
# under /var/lib/microwave. The configuration is read from
# /etc/microwave/microwave.toml (override with `MW_CONFIG`).

ARG RUST_VERSION=1

FROM rust:${RUST_VERSION}-trixie AS builder

WORKDIR /src

# Only the workspace manifest and source are copied. The vendored chef/ and
# chef-server/ trees are excluded by .dockerignore; they are not part of the
# Cargo workspace.
COPY Cargo.toml Cargo.lock ./
COPY bin ./bin
COPY crates ./crates

# Cargo caches live in BuildKit cache mounts so repeated builds do not re-fetch
# the registry. The binaries are staged into /out inside the same RUN because
# /src/target is a cache mount and is not part of the resulting layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
        -p microwave-server \
        -p microwave-ctl-bin \
        -p microwave-migrate-bin \
    && install -D -m 0755 target/release/microwave-server /out/microwave-server \
    && install -D -m 0755 target/release/microwave-ctl /out/microwave-ctl \
    && install -D -m 0755 target/release/chef-server-ctl /out/chef-server-ctl \
    && install -D -m 0755 target/release/microwave-migrate /out/microwave-migrate

FROM debian:trixie-slim AS runtime

ARG UID=10001
ARG GID=10001

LABEL org.opencontainers.image.title="microwave-server" \
      org.opencontainers.image.description="Rust Chef Infra Server API-compatible server" \
      org.opencontainers.image.licenses="AGPL-3.0-only"

# ca-certificates: OpenSearch/S3 over TLS.
# curl: container healthcheck.
# sqlite3: `microwave-ctl psql` for the SQLite backend.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl sqlite3 \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd --system --gid "${GID}" microwave \
    && useradd --system --uid "${UID}" --gid microwave \
        --home-dir /var/lib/microwave --no-create-home \
        --shell /usr/sbin/nologin microwave \
    && install -d -m 0750 -o microwave -g microwave /var/lib/microwave \
    && install -d -m 0755 -o root -g root /etc/microwave

COPY --from=builder /out/microwave-server /usr/local/bin/microwave-server
COPY --from=builder /out/microwave-ctl /usr/local/bin/microwave-ctl
COPY --from=builder /out/chef-server-ctl /usr/local/bin/chef-server-ctl
COPY --from=builder /out/microwave-migrate /usr/local/bin/microwave-migrate

# Baked-in example configuration. Mount a real file over this path in
# production; it is intentionally free of secrets.
COPY --chown=root:microwave --chmod=0640 packaging/microwave.toml.example /etc/microwave/microwave.toml

ENV MW_CONFIG=/etc/microwave/microwave.toml \
    RUST_LOG=info

WORKDIR /var/lib/microwave
USER microwave:microwave

VOLUME ["/var/lib/microwave"]
EXPOSE 8000 8443

# `/_status` is unauthenticated and returns {"status":"pong", ...}. It is a
# liveness probe; it does not check OpenSearch, S3, or the database.
# Set MW_HTTP_PORT when the server binds a port other than 8000 and
# MW_HEALTHCHECK_SCHEME=https when it serves TLS. MW_HEALTHCHECK_HOST overrides
# the probe hostname (default 127.0.0.1) when the certificate only covers a
# DNS name. Set MW_HEALTHCHECK_INSECURE=1 (or true) to skip certificate
# verification for the probe; any other value keeps verification enabled.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD insecure=""; \
        case "${MW_HEALTHCHECK_INSECURE:-}" in 1|true|TRUE) insecure="-k" ;; esac; \
        curl -fsS $insecure \
            "${MW_HEALTHCHECK_SCHEME:-http}://${MW_HEALTHCHECK_HOST:-127.0.0.1}:${MW_HTTP_PORT:-8000}/_status" \
            >/dev/null || exit 1

# No ENTRYPOINT on purpose: `docker run <image> microwave-ctl ...` and
# `docker compose exec microwave microwave-ctl ...` both work without an
# entrypoint override.
#
# The default command goes through the CLI's foreground mode. `microwave-ctl`
# validates the configuration path, resolves `microwave-server`, and then
# `exec`s it on Unix, so the server becomes PID 1. SIGTERM/SIGINT trigger
# graceful shutdown and SIGHUP still reloads the TLS pair, so Docker stop and
# kill signals reach the server directly. `MW_SERVICE_FOREGROUND=1` is the
# environment alternative to `--foreground`. The previous direct command
# `microwave-server serve --config /etc/microwave/microwave.toml` still works
# and can be used as a `command:`/`docker run` override.
CMD ["microwave-ctl", "service", "start", "--foreground", "--config", "/etc/microwave/microwave.toml"]
