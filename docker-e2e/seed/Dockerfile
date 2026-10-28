# Seed image: knife + Berkshelf from the Cinc Workstation package, used to
# download cookbooks from Supermarket, upload them to microwave-server, and
# create the role.
#
# The omnibus package carries its own Ruby, knife, and Berkshelf, so the image
# needs no distro Ruby and no gem installs.
ARG BASE_IMAGE=debian:trixie
FROM ${BASE_IMAGE}

ARG CINC_WORKSTATION_VERSION=26.4.0
ARG CINC_WORKSTATION_URL=https://downloads.cinc.sh/files/stable/cinc-workstation

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
    && rm -rf /var/lib/apt/lists/*

# Pick the Debian architecture package (amd64 or arm64).
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    url="${CINC_WORKSTATION_URL}/${CINC_WORKSTATION_VERSION}/debian/13/cinc-workstation_${CINC_WORKSTATION_VERSION}-1_${arch}.deb"; \
    curl -fsSL "$url" -o /tmp/cinc-workstation.deb; \
    dpkg -i /tmp/cinc-workstation.deb; \
    rm -f /tmp/cinc-workstation.deb; \
    knife --version; \
    berks --version

WORKDIR /seed
COPY seed.sh /seed/seed.sh
# Local wrapper cookbook for the resource-only nginx cookbook. Uploaded next to
# the Supermarket cookbooks and referenced by the role's run_list.
COPY site-cookbooks /seed/site-cookbooks
RUN chmod 0755 /seed/seed.sh

ENTRYPOINT ["/seed/seed.sh"]
