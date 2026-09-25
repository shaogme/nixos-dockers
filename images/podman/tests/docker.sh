#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root_dir="$(cd -- "$script_dir/../../.." && pwd)"
image_file="$root_dir/images/podman/image.nix"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/nixos-dockers-podman-test.XXXXXX")"
container="nixos-dockers-podman-$$"
socket_volume="nixos-dockers-podman-socket-$$"
data_volume="nixos-dockers-podman-data-$$"

cleanup() {
    local status=$?
    docker rm -f "$container" >/dev/null 2>&1 || true
    docker volume rm "$socket_volume" "$data_volume" >/dev/null 2>&1 || true
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

echo "==> starting rootful Podman engine"
engine_run() {
    # crun/netavark need these narrowly scoped policy relaxations for nested
    # rootful workloads; the capability set below remains explicitly bounded.
    docker run \
        --user 0:0 \
        --cap-drop=ALL \
        --cap-add=CHOWN \
        --cap-add=DAC_OVERRIDE \
        --cap-add=FOWNER \
        --cap-add=NET_ADMIN \
        --cap-add=NET_RAW \
        --cap-add=SETFCAP \
        --cap-add=SETGID \
        --cap-add=SETPCAP \
        --cap-add=SETUID \
        --cap-add=SYS_ADMIN \
        --cap-add=SYS_CHROOT \
        --cgroupns=private \
        --security-opt seccomp=unconfined \
        --security-opt apparmor=unconfined \
        --security-opt systempaths=unconfined \
        --device /dev/fuse \
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
    local attempt
    for attempt in {1..60}; do
        if docker exec "$container" /bin/sh -c 'test -S /run/podman/podman.sock'; then
            return 0
        fi
        sleep 1
    done
    docker logs "$container" >&2 || true
    return 1
}
wait_for_socket

echo "==> verifying rootful cgroup and namespace policy"
engine_user="$(docker inspect --format '{{.Config.User}}' "$container")"
[[ "$engine_user" == 0:0 ]]
engine_cgroupns="$(docker inspect --format '{{.HostConfig.CgroupnsMode}}' "$container" 2>/dev/null || true)"
if [[ -n "$engine_cgroupns" ]]; then
    [[ "$engine_cgroupns" == private ]]
fi
engine_cgroup_policy="$(docker exec "$container" /bin/sh -c "grep -E '^(cgroups|cgroupns) =' /etc/containers/containers.conf")"
assert_contains "$engine_cgroup_policy" 'cgroups = "enabled"'
assert_contains "$engine_cgroup_policy" 'cgroupns = "private"'

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
workload_output="$(docker exec "$container" podman --remote --url unix:///run/podman/podman.sock run \
    --rm --network=none --cgroups=enabled --entrypoint /bin/sh podman:latest \
    -c 'test "$(id -u)" = 0; printf rootful-workload')"
[[ "$workload_output" == rootful-workload ]]
docker exec "$container" podman --remote --url unix:///run/podman/podman.sock rm engine-test-workload >/dev/null

echo "==> validating persistent data after engine restart"
docker exec "$container" podman --remote --url unix:///run/podman/podman.sock create \
    --name engine-persistent-workload --entrypoint /bin/sh podman:latest -c 'exit 0' >/dev/null
docker restart "$container" >/dev/null
wait_for_socket
docker exec "$container" podman --remote --url unix:///run/podman/podman.sock inspect engine-persistent-workload >/dev/null
docker exec "$container" podman --remote --url unix:///run/podman/podman.sock rm engine-persistent-workload >/dev/null

echo "Podman engine image tests passed"
