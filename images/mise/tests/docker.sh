#!/usr/bin/env bash
set -Eeuo pipefail

[[ "${NIXOS_DOCKERS_QEMU_GUEST:-}" == 1 && "${NIXOS_DOCKERS_GUEST_RUNTIME:-}" == rootful-docker ]] || {
    echo "mise image fixture requires scripts/qemu-test.sh with rootful-docker" >&2
    exit 2
}

root_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
exec bash "$root_dir/tests/docker-image.sh" mise
