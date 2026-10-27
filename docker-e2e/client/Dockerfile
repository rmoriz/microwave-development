# Test node image: a plain distro with an init system and Cinc Client from
# cinc.sh.
#
# systemd runs as PID 1 (see the compose service flags) and starts the
# cinc-e2e-converge.service oneshot unit. The container therefore stays alive
# after the Chef run, so the converged state can be inspected with
# `docker exec`.
#
# Build with a base image arg, for example:
#   docker build --build-arg BASE_IMAGE=debian:bookworm -t mw-node-debian ./client
#
# Cinc publishes packages for Debian and Ubuntu through omnitruck.cinc.sh.
# Fedora is not an official Cinc target, so Fedora uses the compatible el9
# RPM (Cinc's omnibus package is self-contained).
ARG BASE_IMAGE=debian:bookworm
FROM ${BASE_IMAGE}

ARG CINC_VERSION=19

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN if command -v apt-get >/dev/null 2>&1; then \
        export DEBIAN_FRONTEND=noninteractive; \
        apt-get update \
        && apt-get install -y --no-install-recommends \
            ca-certificates \
            curl \
            procps \
            systemd \
            systemd-sysv \
        && apt-get clean; \
    elif command -v dnf >/dev/null 2>&1; then \
        # Chef's dnf_package provider drives libdnf through a Python helper, so
        # the Python bindings have to be present on Fedora (which ships dnf5
        # and no python3 in the base image).
        dnf install -y ca-certificates curl procps-ng systemd \
            python3 python3-libdnf5 python3-rpm \
        && dnf clean all; \
    else \
        echo "unsupported base image: neither apt-get nor dnf found" >&2; \
        exit 1; \
    fi

RUN set -eux; \
    if command -v apt-get >/dev/null 2>&1; then \
        curl -fsSL https://omnitruck.cinc.sh/install.sh | bash -s -- -v "${CINC_VERSION}"; \
    else \
        arch="$(uname -m)"; \
        url="$(curl -fsS "https://omnitruck.cinc.sh/stable/cinc/metadata?p=el&pv=9&m=${arch}&v=${CINC_VERSION}" | awk '$1 == "url" { print $2 }')"; \
        test -n "$url"; \
        curl -fsSL "$url" -o /tmp/cinc.rpm; \
        dnf install -y /tmp/cinc.rpm; \
        rm -f /tmp/cinc.rpm; \
        dnf clean all; \
    fi; \
    /opt/cinc/bin/cinc-client --version

# The Supermarket `postgres` cookbook depends on `ipaddr_extensions`, whose
# library is compiled into every run and requires the `scanf` gem on Ruby 3.4.
# Install both into Cinc's embedded Ruby so the dependency does not abort the
# run before the role is evaluated.
RUN /opt/cinc/embedded/bin/gem install --no-document scanf ipaddr_extensions

COPY converge.sh /usr/local/bin/cinc-e2e-converge
COPY cinc-e2e-converge.service /etc/systemd/system/cinc-e2e-converge.service
COPY cinc-e2e-reconverge.service /etc/systemd/system/cinc-e2e-reconverge.service
COPY cinc-e2e-converge.path /etc/systemd/system/cinc-e2e-converge.path
RUN chmod 0755 /usr/local/bin/cinc-e2e-converge \
    && mkdir -p /etc/systemd/system/multi-user.target.wants \
    && ln -sf /etc/systemd/system/cinc-e2e-converge.service \
              /etc/systemd/system/multi-user.target.wants/cinc-e2e-converge.service \
    && ln -sf /etc/systemd/system/cinc-e2e-converge.path \
              /etc/systemd/system/multi-user.target.wants/cinc-e2e-converge.path

# systemd-resolved is enabled by Fedora's preset and would take over lookups
# from the glibc resolver. It queries Docker's embedded DNS (127.0.0.11) through
# its own stub and fails to resolve compose service names such as `microwave`,
# while glibc resolves them fine. Mask it everywhere so /etc/resolv.conf is used
# directly.
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
