#!/usr/bin/env bash
set -Eeuo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <image>" >&2
    exit 2
fi

image="$1"
case "$image" in
    mise|npins|rust) ;;
    *)
        echo "unsupported image: $image" >&2
        exit 2
        ;;
esac

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root_dir="$(cd -- "$script_dir/.." && pwd)"
image_file="$root_dir/images/$image/image.nix"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/nixos-dockers-image-test.XXXXXX")"
containers=()

cleanup() {
    local status=$?
    for container in "${containers[@]-}"; do
        docker rm -f "$container" >/dev/null 2>&1 || true
    done
    rm -rf -- "$tmp_dir"
    exit "$status"
}
trap cleanup EXIT

command -v docker >/dev/null
command -v nix-build >/dev/null
command -v nix-instantiate >/dev/null

assert_contains() {
    local value="$1"
    local expected="$2"
    if [[ "$value" != *"$expected"* ]]; then
        echo "expected output to contain: $expected" >&2
        echo "$value" >&2
        return 1
    fi
}

wait_for_running() {
    local container="$1"
    local attempt state
    for attempt in {1..30}; do
        state="$(docker inspect --format '{{.State.Running}}' "$container" 2>/dev/null || true)"
        if [[ "$state" == true ]]; then
            return 0
        fi
        if [[ "$state" == false ]]; then
            echo "container $container exited before becoming ready" >&2
            docker logs "$container" >&2 || true
            return 1
        fi
        sleep 1
    done
    echo "timed out waiting for container $container" >&2
    docker logs "$container" >&2 || true
    return 1
}

test_loaded_image() {
    local attr="$1"
    local archive="$tmp_dir/${attr//\//_}.tar.gz"
    local plan environment handoff

    echo "==> evaluating $image#$attr"
    nix-instantiate --eval --strict "$image_file" -A "$attr.imageVersion" >/dev/null

    echo "==> building $image#$attr"
    nix-build --no-out-link "$image_file" -A "$attr" -o "$archive"

    echo "==> loading $attr:latest"
    docker load --input "$archive"

    echo "==> validating configured entrypoint"
    handoff="$(docker run --rm --env RUN_AS_ROOT=1 "$attr:latest" /bin/printf 'nixos-docker entrypoint')"
    assert_contains "$handoff" 'nixos-docker entrypoint'

    local tool_output tool
    case "$image" in
        mise) tool=mise ;;
        npins) tool=npins ;;
        rust) tool=rustc ;;
    esac
    echo "==> validating image tool ($tool)"
    tool_output="$(docker run --rm --entrypoint /bin/sh "$attr:latest" -c "command -v $tool && $tool --version")"
    assert_contains "$tool_output" "$tool"

    echo "==> validating container-init plan"
    plan="$(docker run --rm --entrypoint /usr/bin/container-init "$attr:latest" plan --json)"
    assert_contains "$plan" '"actions"'
    assert_contains "$plan" '"handoff"'

    echo "==> validating dev-env materialization"
    environment="$(docker run --rm --entrypoint /usr/bin/dev-env "$attr:latest" print --format json)"
    assert_contains "$environment" '"PATH"'
    assert_contains "$environment" '"NIX_PATH"'

    echo "==> validating runtime handoff"
    handoff="$(docker run --rm --env RUN_AS_ROOT=1 --entrypoint /usr/bin/container-init "$attr:latest" run -- /bin/printf 'nixos-docker handoff')"
    assert_contains "$handoff" 'nixos-docker handoff'

    echo "==> validating login-shell shim"
    handoff="$(docker run --rm --entrypoint /usr/bin/dev-env-login-shell "$attr:latest" -c 'printf "nixos-docker login-shell"')"
    assert_contains "$handoff" 'nixos-docker login-shell'

    echo "==> validating non-mounted workspace default"
    handoff="$(docker run --rm --entrypoint /usr/bin/container-init "$attr:latest" run -- /bin/sh -c 'test "$HOME" = /home/dev && test "$USER" = dev && test "$LOGNAME" = dev && test "$(id -u)" = 1000 && test "$(id -g)" = 1000 && test "$(stat -c %u:%g /home/dev)" = "1000:1000" && test "$(stat -c %U /home/dev)" = dev')"
    [[ -z "$handoff" ]]

    echo "==> validating root identity handoff"
    handoff="$(docker run --rm --env RUN_AS_ROOT=1 --entrypoint /usr/bin/container-init "$attr:latest" run -- /bin/sh -c 'test "$HOME" = /root && test "$USER" = root && test "$(id -u)" = 0 && test "$(id -g)" = 0 && test "$(stat -c %u:%g /root)" = "0:0" && test "$(stat -c %U /root)" = root')"
    [[ -z "$handoff" ]]

    echo "==> validating non-root identity handoff"
    # Pick a parent ID that maps to container ID 1 in both maps. This keeps
    # the integration check valid for rootful engines and for the partially
    # mapped user namespaces used by rootless Podman.
    local identity_fixture host_uid host_gid expected_uid expected_gid
    identity_fixture="$(docker run --rm --entrypoint /bin/sh "$attr:latest" -c '
        uid_parent="$(awk '\''$1 <= 1 && 1 < $1 + $3 { print $2 + (1 - $1); exit}'\'' /proc/self/uid_map)"
        gid_parent="$(awk '\''$1 <= 1 && 1 < $1 + $3 { print $2 + (1 - $1); exit}'\'' /proc/self/gid_map)"
        test -n "$uid_parent" && test -n "$gid_parent"
        printf "%s:%s:1:1\\n" "$uid_parent" "$gid_parent"
    ')"
    IFS=: read -r host_uid host_gid expected_uid expected_gid <<< "$identity_fixture"
    handoff="$(docker run --rm --env HOST_UID="$host_uid:$host_gid" --env EXPECTED_UID="$expected_uid" --env EXPECTED_GID="$expected_gid" --entrypoint /usr/bin/container-init "$attr:latest" run -- /bin/sh -c 'test "$HOME" = /home/dev && test "$USER" = dev && test "$LOGNAME" = dev && test "$(id -u)" = "$EXPECTED_UID" && test "$(id -g)" = "$EXPECTED_GID" && test "$(stat -c %u:%g /home/dev)" = "$EXPECTED_UID:$EXPECTED_GID"')"
    [[ -z "$handoff" ]]

    local exec_container="nixos-dockers-${image}-${attr//[^a-zA-Z0-9_.-]/-}-exec-$$"
    local exec_output low_level root_output
    echo "==> validating root docker exec Bash bootstrap ($exec_container)"
    containers+=("$exec_container")
    docker run --detach --name "$exec_container" \
        --env HOST_UID="$host_uid:$host_gid" \
        "$attr:latest" /bin/sleep 300 >/dev/null
    wait_for_running "$exec_container"

    exec_output="$(docker exec --env EXPECTED_UID="$expected_uid" --env EXPECTED_GID="$expected_gid" "$exec_container" bash -lc \
        'test "$USER" = dev && test "$HOME" = /home/dev && test "$(id -u)" = "$EXPECTED_UID" && test "$(id -g)" = "$EXPECTED_GID" && test "$(stat -c %u:%g /home/dev)" = "$EXPECTED_UID:$EXPECTED_GID"')"
    [[ -z "$exec_output" ]]

    exec_output="$(docker exec --env EXPECTED_UID="$expected_uid" --env EXPECTED_GID="$expected_gid" "$exec_container" /bin/bash -lc \
        'test "$USER" = dev && test "$HOME" = /home/dev && test "$(id -u)" = "$EXPECTED_UID" && test "$(id -g)" = "$EXPECTED_GID" && test "$(stat -c %u:%g /home/dev)" = "$EXPECTED_UID:$EXPECTED_GID"')"
    [[ -z "$exec_output" ]]

    root_output="$(docker exec -e RUN_AS_ROOT=1 "$exec_container" bash -lc \
        'test "$USER" = root && test "$HOME" = /root && test "$(id -u)" = 0 && test "$(id -g)" = 0 && test "$(stat -c %u:%g /root)" = "0:0" && test "$(stat -c %U /root)" = root')"
    [[ -z "$root_output" ]]

    low_level="$(docker exec "$exec_container" /bin/sh -c 'test "$(id -u)" = 0 && printf "%s" "${BASH-unset}"')"
    [[ "$low_level" == /bin/sh ]]

    docker rm -f "$exec_container" >/dev/null
    unset 'containers[-1]'

    if [[ "$attr" == vscode-* ]]; then
        local container="nixos-dockers-${image}-$$"
        echo "==> validating default service deployment ($container)"
        containers+=("$container")
        docker run --detach --name "$container" --env RUN_AS_ROOT=1 "$attr:latest" >/dev/null
        wait_for_running "$container"
        environment="$(docker exec "$container" /usr/bin/dev-env print --format json)"
        assert_contains "$environment" '"PATH"'
        echo "==> validating root SSH login shell"
        handoff="$(docker exec "$container" /bin/sh -c \
            'test "$(awk -F: '\''$1 == "root" { print $7; exit }'\'' /etc/passwd)" = /usr/bin/dev-env-login-shell')"
        [[ -z "$handoff" ]]
        handoff="$(docker exec "$container" /usr/bin/dev-env-login-shell -c \
            'test "$HOME" = /root && test "$(id -u)" = 0 && test "$(id -g)" = 0 && test "$(stat -c %u:%g /root)" = "0:0" && test "$(stat -c %U /root)" = root')"
        [[ -z "$handoff" ]]
        docker rm -f "$container" >/dev/null
        unset 'containers[-1]'
    fi
}

test_builder_image() {
    local attr="${image}-builder"
    local archive="$tmp_dir/${attr//\//_}.tar.gz"
    local entrypoint tool tool_output

    echo "==> evaluating $image#$attr"
    nix-instantiate --eval --strict "$image_file" -A "$attr.imageVersion" >/dev/null

    local runtime_version builder_version
    runtime_version="$(nix-instantiate --eval --raw "$image_file" -A "$image.imageVersion")"
    builder_version="$(nix-instantiate --eval --raw "$image_file" -A "$attr.imageVersion")"
    [[ "$runtime_version" == "$builder_version" ]]

    echo "==> building $image#$attr"
    nix-build --no-out-link "$image_file" -A "$attr" -o "$archive"

    echo "==> loading $attr:latest"
    docker load --input "$archive"

    echo "==> validating builder metadata"
    entrypoint="$(docker inspect --format '{{json .Config.Entrypoint}}' "$attr:latest")"
    [[ "$entrypoint" == "null" || "$entrypoint" == "[]" ]]

    case "$image" in
        mise) tool=mise ;;
        npins) tool=npins ;;
        rust) tool=rustc ;;
    esac
    echo "==> validating builder tool ($tool)"
    tool_output="$(docker run --rm --entrypoint /bin/bash "$attr:latest" -c "
        set -euo pipefail
        test \"\$(readlink -f /bin/bash)\" != /usr/bin/dev-env
        test \"\$(readlink -f /usr/bin/bash)\" != /usr/bin/dev-env
        test ! -e /usr/bin/container-init
        test ! -e /usr/bin/dev-env
        command -v $tool
        $tool --version
        printf '%s\\n' '#!/usr/bin/env bash' 'set -eu' 'printf builder-shebang' > /tmp/builder-shebang
        chmod +x /tmp/builder-shebang
        /tmp/builder-shebang
    ")"
    assert_contains "$tool_output" "$tool"
    assert_contains "$tool_output" builder-shebang
}

test_loaded_image "$image"
test_loaded_image "vscode-$image"
test_builder_image

echo "Docker image tests passed: $image and vscode-$image"
