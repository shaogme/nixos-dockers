#!/bin/sh
set -eu

test_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
crate_dir=$(CDPATH= cd -- "$test_dir/.." && pwd)
image_name=${DEV_ENV_PROVIDER_TEST_IMAGE:-dev-env-provider-test}

docker build --tag "$image_name" --file "$crate_dir/Dockerfile.test" "$crate_dir/../.."
docker run --rm "$image_name"
