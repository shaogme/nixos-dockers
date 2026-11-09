#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
test_image=${TEST_WORKSPACE_IMAGE:-nixos-dockers-workspace-test}

docker build --tag "$test_image" --file "$script_dir/Dockerfile.test" "$repo_root"
docker run --rm --privileged "$test_image"
