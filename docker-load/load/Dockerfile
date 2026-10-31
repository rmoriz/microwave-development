# Load driver image: Cinc Client plus the built `microwave-load` binary.
#
# Build it through Compose so the tags stay consistent with the stack:
#   cd docker-load && docker compose --profile smoke build
#
# The container is a one-shot: `seed` fills the store, `run` drives the HTTP
# converges and the canary. It runs as root because the `keys`/`out` bind
# mounts belong to the host user and the seed has to write into the server's
# data volume.
ARG RUST_VERSION=1
FROM rust:${RUST_VERSION}-trixie AS builder

WORKDIR /src

# Only the workspace manifest and source are copied; the vendored chef/ and
# chef-server/ trees are excluded by .dockerignore.
COPY Cargo.toml Cargo.lock ./
COPY bin ./bin
COPY crates ./crates

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p microwave-load-bin \
    && install -D -m 0755 target/release/microwave-load /out/microwave-load

FROM debian:trixie-slim

ARG CINC_VERSION=19

# UID the server runs as; the seed hands the data volume back to it.
ARG UID=10001

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

# curl: presigned /bookshelf uploads and the omnitruck download.
# procps: `ps` for ohai during the canary converge.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl procps \
    && rm -rf /var/lib/apt/lists/*

# Cinc Client from cinc.sh, installed exactly as docker-e2e/client/Dockerfile
# does on Debian.
RUN set -eux; \
    curl -fsSL https://omnitruck.cinc.sh/install.sh | bash -s -- -v "${CINC_VERSION}"; \
    /opt/cinc/bin/cinc-client --version

# The canary runs the default `--chef-client chef-client`.
RUN ln -s /opt/cinc/bin/cinc-client /usr/local/bin/chef-client

COPY --from=builder /out/microwave-load /usr/local/bin/microwave-load

# Mount points for the key manifest and the report, and the server's data
# directory the seed writes cookbook blobs into.
RUN mkdir -p /keys /out /var/lib/microwave

WORKDIR /work

ENTRYPOINT ["microwave-load"]
