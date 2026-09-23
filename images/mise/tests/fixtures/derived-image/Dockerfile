ARG NIXOS_DOCKERS_VERSION=latest

FROM mise-builder:${NIXOS_DOCKERS_VERSION} AS mise-tools

RUN set -eu; \
    mkdir -p /etc/mise /usr/local/share/mise /data/cache/mise; \
    mkdir -p /tmp/mise-fixture; \
    cd /tmp/mise-fixture; \
    mise trust --all; \
    mise lock --global --platform linux-x64,linux-arm64; \
    mise install; \
    printf '%s\n' builder-artifact > /data/cache/mise/fixture-tool

FROM mise:${NIXOS_DOCKERS_VERSION}

COPY --from=mise-tools /etc/mise /etc/mise
COPY --from=mise-tools /usr/local/share/mise /usr/local/share/mise
COPY --from=mise-tools /data/cache/mise /data/cache/mise
