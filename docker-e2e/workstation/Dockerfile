# Workstation image: an interactive box for driving the e2e stack by hand.
#
# systemd runs as PID 1 (see the compose service flags) and starts the
# workstation-prepare.service oneshot, which stages the knife config the seed
# wrote. The container stays up so commands can be run with `docker exec`.
#
# knife and Berkshelf come from the Cinc Workstation package; OpenCode is
# installed from the official installer and configured against the stack's MCP
# endpoint in /work.
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
        systemd \
        systemd-sysv \
        vim-tiny \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

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
    && mv /root/.opencode/bin/opencode /usr/local/bin/opencode \
    && opencode --version | grep -qx "opencode v${OPENCODE_VERSION}" \
    && opencode --version

COPY workstation-prepare.sh /usr/local/bin/workstation-prepare
COPY workstation-prepare.service /etc/systemd/system/workstation-prepare.service
RUN chmod 0755 /usr/local/bin/workstation-prepare \
    && mkdir -p /etc/systemd/system/multi-user.target.wants /work \
    && ln -sf /etc/systemd/system/workstation-prepare.service \
              /etc/systemd/system/multi-user.target.wants/workstation-prepare.service

COPY opencode.json /work/opencode.json
COPY AGENTS.md /work/AGENTS.md

# Every exec session starts here so OpenCode picks up the project config.
WORKDIR /work

# The e2e token is baked in so an exec session needs no prefix. Compose can
# still override it with MW_MCP_TOKEN.
ENV MW_MCP_TOKEN=e2e-mcp-token

# knife 19 only reads ~/.chef/knife.rb through KNIFE_HOME, not through HOME
# alone, and systemd units have no HOME at all.
ENV KNIFE_HOME=/root/.chef

# systemd-resolved is enabled by some distro presets and would take over
# lookups from the glibc resolver. Mask it so Docker's embedded DNS is used.
RUN systemctl mask systemd-resolved

# Container images have no virtual terminals, so getty@tty1..6 fail and make
# systemd report "degraded". Mask them; console-getty on /dev/console still
# offers a login prompt when the container is attached to a TTY.
RUN systemctl mask getty@tty1.service getty@tty2.service getty@tty3.service \
                    getty@tty4.service getty@tty5.service getty@tty6.service \
                    getty-static.service

# systemd as PID 1: send the container stop signal systemd understands.
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
