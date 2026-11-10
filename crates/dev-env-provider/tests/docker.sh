#!/usr/bin/env bash
set -Eeuo pipefail

if [[ "${NIXOS_DOCKERS_QEMU_GUEST:-}" != 1 ]] || \
    [[ "${NIXOS_DOCKERS_GUEST_RUNTIME:-}" != rootful-docker && "${NIXOS_DOCKERS_GUEST_RUNTIME:-}" != rootless-podman ]]; then
    echo "dev-env-provider fixture requires the crates QEMU suite" >&2
    exit 2
fi

test_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
crate_dir="$(cd -- "$test_dir/.." && pwd)"
docker build --tag dev-env-provider-test --file "$crate_dir/Dockerfile.test" "$crate_dir/../.."
docker run --rm --env "NIXOS_DOCKERS_GUEST_RUNTIME=$NIXOS_DOCKERS_GUEST_RUNTIME" dev-env-provider-test
