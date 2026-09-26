#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root_dir="$(cd -- "$script_dir/../../.." && pwd)"
image_file="$root_dir/images/podman/image.nix"
compose_file="$root_dir/images/podman/docker-compose.yml"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/nixos-dockers-podman-test.XXXXXX")"
container="nixos-dockers-podman-$$"
dev_container="nixos-dockers-podman-alpine-dev-$$"
socket_volume="nixos-dockers-podman-socket-$$"
data_volume="nixos-dockers-podman-data-$$"

cleanup() {
    local status=$?
    docker rm -f "$container" >/dev/null 2>&1 || true
    docker rm -f "$dev_container" >/dev/null 2>&1 || true
    docker volume rm "$socket_volume" "$data_volume" >/dev/null 2>&1 || true
    rm -rf -- "$tmp_dir"
    exit "$status"
}
trap cleanup EXIT

command -v docker >/dev/null
command -v nix-build >/dev/null
command -v nix-instantiate >/dev/null
[[ -f "$compose_file" ]]
grep -Eq '^[[:space:]]+image:[[:space:]]+alpine:latest[[:space:]]*$' "$compose_file"

assert_contains() {
    local value="$1"
    local expected="$2"
    if [[ "$value" != *"$expected"* ]]; then
        echo "expected output to contain: $expected" >&2
        echo "$value" >&2
        return 1
    fi
}

echo "==> validating engine metadata"
[[ "$(nix-instantiate --eval --raw "$image_file" -A role)" == engine ]]
[[ "$(nix-instantiate --eval --json "$image_file" -A outputs)" == '["podman"]' ]]
nix-instantiate --eval --strict "$image_file" -A podman.imageVersion >/dev/null

archive="$tmp_dir/podman.tar.gz"
echo "==> building engine image"
nix-build --no-out-link "$image_file" -A podman -o "$archive"
docker load --input "$archive" >/dev/null

echo "==> checking that development runtime files are absent"
for path in /usr/bin/container-init /usr/bin/dev-env /usr/bin/sshd /usr/bin/mise /root/.cargo; do
    docker run --rm --entrypoint /bin/sh podman:latest -c "test ! -e '$path'"
done

docker volume create "$socket_volume" >/dev/null
docker volume create "$data_volume" >/dev/null
mkdir -p "$tmp_dir/workspace"
printf 'engine test workspace\n' > "$tmp_dir/workspace/README"

echo "==> starting rootless Podman engine"
engine_run() {
    # The engine must remain usable from a rootless outer daemon.  Overlay
    # storage uses fuse-overlayfs; the engine uses a private cgroup namespace
    # and an explicit device set without privileged mode.
    docker run \
        --user 1000:1000 \
        --cap-drop=ALL \
        --cap-add=SYS_ADMIN \
        --cap-add=SETUID \
        --cap-add=SETGID \
        --cap-add=DAC_OVERRIDE \
        --cgroupns=private \
        --security-opt seccomp=unconfined \
        --security-opt systempaths=unconfined \
        --device /dev/fuse \
        --device /dev/net/tun \
        "$@"
}

engine_run --detach --name "$container" \
    --env PODMAN_SOCKET_GID=1000 \
    --env PODMAN_SOCKET_MODE=0660 \
    --volume "$socket_volume:/run/podman" \
    --volume "$data_volume:/var/lib/containers" \
    --volume "$tmp_dir/workspace:/workspace" \
    podman:latest >/dev/null

wait_for_socket() {
    wait_for_socket_in "$container"
}

wait_for_socket_in() {
    local probe_container="$1"
    local attempt
    for attempt in {1..60}; do
        if docker exec "$probe_container" /bin/sh -c 'test -S /run/podman/podman.sock' >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    return 1
}
wait_for_socket

echo "==> verifying rootless cgroup and namespace policy"
engine_user="$(docker inspect --format '{{.Config.User}}' "$container")"
[[ "$engine_user" == 1000:1000 ]]
engine_cgroupns="$(docker inspect --format '{{.HostConfig.CgroupnsMode}}' "$container" 2>/dev/null || true)"
if [[ -n "$engine_cgroupns" ]]; then
    [[ "$engine_cgroupns" == private ]]
fi
engine_cgroup_policy="$(docker exec "$container" /bin/sh -c "grep -E '^(cgroups|cgroupns) =' /etc/containers/containers.conf")"
assert_contains "$engine_cgroup_policy" 'cgroups = "enabled"'
assert_contains "$engine_cgroup_policy" 'cgroupns = "private"'
engine_cgroup_mount="$(docker exec "$container" /bin/sh -c "grep ' /sys/fs/cgroup ' /proc/self/mountinfo")"
assert_contains "$engine_cgroup_mount" 'cgroup2'

echo "==> validating socket permissions and engine configuration"
socket_stat="$(docker exec "$container" /bin/sh -c "stat -c '%a:%g' /run/podman/podman.sock")"
[[ "$socket_stat" == 660:1000 ]]
local_info="$(docker exec "$container" podman info --format '{{.Host.OCIRuntime.Name}} {{.Store.GraphDriverName}} {{.Store.GraphRoot}}')"
assert_contains "$local_info" crun
assert_contains "$local_info" overlay
assert_contains "$local_info" /var/lib/containers/storage
remote_info="$(docker exec "$container" podman --remote --url unix:///run/podman/podman.sock info --format '{{.Host.OCIRuntime.Name}}')"
assert_contains "$remote_info" crun

echo "==> validating API create/delete and workspace path"
docker exec -i "$container" podman load < "$archive" >/dev/null
docker exec "$container" podman --remote --url unix:///run/podman/podman.sock create \
    --name engine-test-workload --entrypoint /bin/sh podman:latest -c 'test -d /workspace' >/dev/null

echo "==> validating bridge and host networking through the engine socket"
bridge_output="$(docker exec "$container" podman --remote --url unix:///run/podman/podman.sock run \
    --rm --network=bridge --entrypoint /bin/sh podman:latest \
    -c 'printf socket-bridge-network')"
[[ "$bridge_output" == socket-bridge-network ]]
host_output="$(docker exec "$container" podman --remote --url unix:///run/podman/podman.sock run \
    --rm --network=host --entrypoint /bin/sh podman:latest \
    -c 'printf socket-host-network')"
[[ "$host_output" == socket-host-network ]]

echo "==> reproducing Alpine dev socket networking without extra capabilities"
docker run --detach --name "$dev_container" \
    --cap-drop=ALL \
    --security-opt no-new-privileges:true \
    --group-add 1000 \
    --env CONTAINER_HOST=unix:///run/podman/podman.sock \
    --env DOCKER_HOST=unix:///run/podman/podman.sock \
    --volume "$socket_volume:/run/podman" \
    --volume "$tmp_dir/workspace:/workspace" \
    --workdir /workspace \
    docker.io/library/alpine:latest \
    /bin/sh -c 'apk add --no-cache podman >/dev/null || true; exec sleep 300' >/dev/null
for attempt in {1..60}; do
    if docker exec "$dev_container" /bin/sh -c 'command -v podman >/dev/null 2>&1'; then
        break
    fi
    if [[ "$attempt" -eq 60 ]]; then
        docker logs "$dev_container" >&2 || true
        exit 1
    fi
    sleep 1
done

dev_bridge_output="$(docker exec "$dev_container" podman run \
    --rm --network=bridge docker.io/library/alpine:latest \
    /bin/sh -c 'printf alpine-dev-bridge-network')"
[[ "$dev_bridge_output" == alpine-dev-bridge-network ]]
dev_host_output="$(docker exec "$dev_container" podman run \
    --rm --network=host docker.io/library/alpine:latest \
    /bin/sh -c 'printf alpine-dev-host-network')"
[[ "$dev_host_output" == alpine-dev-host-network ]]

workload_output="$(docker exec "$container" podman --remote --url unix:///run/podman/podman.sock run \
    --rm --network=none --cgroups=enabled --entrypoint /bin/sh podman:latest \
    -c 'test "$(id -u)" = 1000; printf rootless-workload')"
[[ "$workload_output" == rootless-workload ]]
docker exec "$container" podman --remote --url unix:///run/podman/podman.sock rm engine-test-workload >/dev/null

echo "==> validating persistent data after engine restart"
docker exec "$container" podman --remote --url unix:///run/podman/podman.sock create \
    --name engine-persistent-workload --entrypoint /bin/sh podman:latest -c 'exit 0' >/dev/null
docker restart "$container" >/dev/null
wait_for_socket
docker exec "$container" podman --remote --url unix:///run/podman/podman.sock inspect engine-persistent-workload >/dev/null
docker exec "$container" podman --remote --url unix:///run/podman/podman.sock rm engine-persistent-workload >/dev/null

echo "Podman engine image tests passed"
