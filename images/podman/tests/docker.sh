#!/usr/bin/env bash
set -Eeuo pipefail

if [[ "${NIXOS_DOCKERS_QEMU_GUEST:-}" != 1 ]]; then
    echo "Podman image tests must run through scripts/qemu-test.sh" >&2
    exit 2
fi

case "${NIXOS_DOCKERS_GUEST_RUNTIME:-}" in
    rootful-docker)
        [[ "$(id -u)" == 0 ]]
        docker info >/dev/null
        ;;
    rootless-podman)
        [[ "$(id -u)" == 1000 && "$(id -g)" == 1000 ]]
        [[ "$(podman info --format '{{.Host.Security.Rootless}}')" == true ]]
        graphroot="$(podman info --format '{{.Store.GraphRoot}}')"
        [[ "$graphroot" == /home/podman/.local/share/containers/storage && -w "$graphroot" ]]
        ;;
    *)
        echo "unsupported Podman test runtime: ${NIXOS_DOCKERS_GUEST_RUNTIME:-<empty>}" >&2
        exit 2
        ;;
esac

exec bash "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
