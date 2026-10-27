# Seed image: knife + Berkshelf, used to download cookbooks from Supermarket,
# upload them to microwave-server, and create the role.
FROM ruby:3.4-bookworm

ARG KNIFE_VERSION=18.8.68
ARG BERKSHELF_VERSION=8.0.9

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        git \
    && rm -rf /var/lib/apt/lists/*

# The `knife` gem is separate from the chef-client omnibus package and carries
# the Supermarket/upload subcommands we need. Berkshelf 8 still requires the
# pre-1.0 `archive/tar/minitar` namespace, so minitar is pinned to 0.9.
RUN gem install --no-document \
        "knife:${KNIFE_VERSION}" \
        "berkshelf:${BERKSHELF_VERSION}" \
        "minitar:0.9" \
    && gem uninstall --ignore-dependencies --executables --all minitar --version 1.1.0 \
    && gem list minitar

WORKDIR /seed
COPY seed.sh /seed/seed.sh
# Local wrapper cookbook for the resource-only nginx cookbook. Uploaded next to
# the Supermarket cookbooks and referenced by the role's run_list.
COPY site-cookbooks /seed/site-cookbooks
RUN chmod 0755 /seed/seed.sh

ENTRYPOINT ["/seed/seed.sh"]
