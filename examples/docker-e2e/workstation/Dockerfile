# Workstation image: an interactive box for driving the e2e stack by hand.
#
# There is no init system here. The entrypoint runs the prepare script, which
# stages the knife config the seed wrote and warms the MCP client, then keeps
# the container alive for `docker compose exec` and `./shell`.
#
# knife and Berkshelf come from the Cinc Workstation package; OpenCode is
# installed from the official installer and configured against the stack's MCP
# endpoint in /root/work.
#
# Everything runs as root: the box is disposable and only talks to the local
# e2e stack.
ARG BASE_IMAGE=debian:trixie
FROM ${BASE_IMAGE}

ARG CINC_WORKSTATION_VERSION=26.4.0
ARG CINC_WORKSTATION_URL=https://downloads.cinc.sh/files/stable/cinc-workstation

# Pinned: the installer layer is cached by command text, and the MCP warm-up in
# workstation-prepare depends on 2.2.0's reload behaviour.
ARG OPENCODE_VERSION=2.2.0

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        gnupg \
        less \
        procps \
        vim-tiny \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/* \
    && install -d \
        /root/work \
        /root/work/.chef \
        /root/.local/share/opencode \
        /root/.local/state/opencode \
        /root/.config/opencode \
        /var/tmp/opencode

# Pick the Debian architecture package (amd64 or arm64).
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    url="${CINC_WORKSTATION_URL}/${CINC_WORKSTATION_VERSION}/debian/13/cinc-workstation_${CINC_WORKSTATION_VERSION}-1_${arch}.deb"; \
    curl -fsSL "$url" -o /tmp/cinc-workstation.deb; \
    dpkg -i /tmp/cinc-workstation.deb; \
    rm -f /tmp/cinc-workstation.deb; \
    knife --version

RUN curl -fsSL https://opencode.ai/v2/install \
        | bash -s -- --no-modify-path --version "$OPENCODE_VERSION" \
    && install -D -m 0755 /root/.opencode/bin/opencode /usr/local/libexec/opencode \
    && /usr/local/libexec/opencode --version | grep -qx "opencode v${OPENCODE_VERSION}" \
    && /usr/local/libexec/opencode --version

# The installer drops the binary under /root; expose it on PATH for exec sessions.
RUN ln -sf /usr/local/libexec/opencode /usr/local/bin/opencode

COPY workstation-prepare.sh /usr/local/bin/workstation-prepare
COPY entrypoint.sh /usr/local/bin/workstation-entrypoint
RUN chmod 0755 /usr/local/bin/workstation-prepare \
              /usr/local/bin/workstation-entrypoint

COPY opencode.json /root/work/opencode.json
COPY AGENTS.md /root/work/AGENTS.md

# Every exec session starts here so OpenCode picks up the project config.
WORKDIR /root/work

# The e2e token is baked in so an exec session needs no prefix. Compose can
# still override it with MW_MCP_TOKEN.
ENV MW_MCP_TOKEN=e2e-mcp-token

# knife 19 only reads knife.rb through KNIFE_HOME, not through HOME alone. The
# config lives in /root/work so it sits next to the project files and is
# visible in the interactive shell.
ENV KNIFE_HOME=/root/work/.chef

# OpenCode writes scratch state below TMPDIR; keep it on the image layer instead
# of the container's /tmp.
ENV TMPDIR=/var/tmp/opencode

# Prepare once, then sleep so exec sessions have a container to attach to.
ENTRYPOINT ["/usr/local/bin/workstation-entrypoint"]
