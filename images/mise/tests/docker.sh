#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root_dir="$(cd -- "$script_dir/../../.." && pwd)"

"$root_dir/tests/docker-image.sh" mise

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/nixos-dockers-mise-derived-test.XXXXXX")"

cleanup() {
    local status=$?
    rm -rf -- "$tmp_dir"
    exit "$status"
}
trap cleanup EXIT

command -v docker >/dev/null
runtime_failure=""
if runtime_failure="$(docker run --rm --entrypoint /bin/bash mise:latest -c 'printf runtime-shell-should-not-start' 2>&1)"; then
    echo "runtime Bash unexpectedly bypassed the container-init backend" >&2
    exit 1
fi
[[ "$runtime_failure" == *backend* ]]

derived_image="mise-derived:test"
fixture_dir="$script_dir/fixtures/derived-image"

echo "==> building derived mise image"
docker build --tag "$derived_image" "$fixture_dir"

echo "==> validating builder artifact copy and runtime handoff"
docker run --rm --entrypoint /bin/sh "$derived_image" -c '
    set -eu
    test "$(cat /data/cache/mise/fixture-tool)" = builder-artifact
    test "$(readlink /bin/bash)" = /usr/bin/dev-env
    test "$(readlink /usr/bin/bash)" = dev-env
'
handoff="$(docker run --rm --env RUN_AS_ROOT=1 "$derived_image" /bin/printf 'derived-runtime-handoff')"
[[ "$handoff" == derived-runtime-handoff ]]

echo "Mise derived image tests passed"
