#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

for script in podman-rootless-test.sh podman-docker-test.sh; do
    [[ -f "$script_dir/$script" ]] || {
        echo "test script not found: $script_dir/$script" >&2
        exit 1
    }
done

echo "==> running rootless Podman runtime test"
bash "$script_dir/podman-rootless-test.sh"

echo "==> running Podman Docker runtime test"
bash "$script_dir/podman-docker-test.sh"

echo "Podman runtime tests passed"
