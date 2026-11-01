# A backend node of the split fixture stack: Debian 13 with the same Cinc
# release the kitchen node converges with. The cookbook and the fixture
# cookbook are mounted at converge time, so this image only carries the client.
FROM debian:13

ARG CINC_VERSION=19.4.46

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl gnupg openssl procps \
 # The base image's docker-clean hook wipes /var/cache/apt/archives after every
 # run; that directory is a persistent host mount here, so drop the hook and
 # keep the downloaded packages.
 && rm -f /etc/apt/apt.conf.d/docker-clean \
 && printf 'APT::Keep-Downloaded-Packages "true";\nBinary::apt::APT::Keep-Downloaded-Packages "true";\n' \
      > /etc/apt/apt.conf.d/99keep-packages \
 && install -d -m 0700 /root/.gnupg \
 && curl -fsSLo /tmp/cinc.deb \
      "https://packages.cinc.sh/files/stable/cinc/${CINC_VERSION}/debian/13/cinc_${CINC_VERSION}-1_$(dpkg --print-architecture).deb" \
 && dpkg -i /tmp/cinc.deb \
 && rm -f /tmp/cinc.deb \
 && rm -rf /var/lib/apt/lists/*

ENV CHEF_LICENSE=accept-silent

CMD ["sleep", "infinity"]
