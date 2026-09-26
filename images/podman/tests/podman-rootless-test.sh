#!/usr/bin/env bash
set -Eeuo pipefail

# Build a NixOS QEMU guest whose outer container runtime is rootless Podman,
# then run the Podman engine image test as the guest's unprivileged user.

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/../../.." && pwd)"
image_nix="$script_dir/podman-rootless-image.nix"
ssh_port="${PODMAN_ROOTLESS_QEMU_SSH_PORT:-22231}"
memory="${PODMAN_ROOTLESS_QEMU_MEMORY:-4096}"
cores="${PODMAN_ROOTLESS_QEMU_CORES:-4}"
work_dir="${PODMAN_ROOTLESS_QEMU_WORK_DIR:-${TMPDIR:-/tmp}/nixos-dockers-podman-rootless-qemu-test}"
qemu_log="$work_dir/qemu.log"
image_link="$work_dir/image"
image_path=""
vm_link="$work_dir/vm"
vm_disk="$work_dir/vm.qcow2"
vm_runner=""
qemu_pid=""
known_hosts="$work_dir/known_hosts"

require_command() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "required command not found: $1" >&2
        exit 127
    }
}

for command in nix-build nix-instantiate qemu-img ssh sshpass tar; do
    require_command "$command"
done

mkdir -p "$work_dir"
rm -f "$known_hosts"

# Resolve the pinned nixpkgs source when this script is run without a caller
# that has already configured NIX_PATH.
if [[ -z "${NIX_PATH:-}" ]]; then
    nixpkgs_path="$(nix-instantiate --eval --raw -E \
        "let sources = import $script_dir/../npins; in sources.nixpkgs.outPath")"
    export NIX_PATH="nixpkgs=$nixpkgs_path"
fi

cleanup() {
    local status=$?
    if [[ -n "$qemu_pid" ]] && kill -0 "$qemu_pid" 2>/dev/null; then
        kill "$qemu_pid" 2>/dev/null || true
        wait "$qemu_pid" 2>/dev/null || true
    fi
    if [[ "$status" -ne 0 && -f "$qemu_log" ]]; then
        echo "==> complete QEMU log: $qemu_log" >&2
        cat "$qemu_log" >&2 || true
    fi
    exit "$status"
}
trap cleanup EXIT

echo "==> building the rootless Podman NixOS qcow2 image"
nix-build --no-out-link "$image_nix" -o "$image_link"
image_path="$(readlink -f "$image_link")"
if [[ -d "$image_path" ]]; then
    image_path="$(find "$image_path" -maxdepth 1 -type f -name '*.qcow2' -print -quit)"
fi
[[ -n "$image_path" && -f "$image_path" ]] || {
    echo "could not find a qcow2 image in $image_link" >&2
    exit 1
}

echo "==> building the NixOS VM runner"
nix-build --no-out-link "$image_nix" --argstr output metadata -A vm -o "$vm_link"
rm -f "$vm_disk"
qemu-img create -f qcow2 -F qcow2 -b "$image_path" "$vm_disk" >/dev/null
vm_runner="$(readlink -f "$vm_link")/bin/run-nixos-vm"
[[ -x "$vm_runner" && -f "$vm_disk" ]] || {
    echo "VM runner setup is incomplete" >&2
    exit 1
}

echo "==> starting rootless Podman NixOS VM"
(
    cd "$work_dir"
    QEMU_NET_OPTS="hostfwd=tcp:127.0.0.1:$ssh_port-:22" \
        NIX_DISK_IMAGE="$vm_disk" \
        QEMU_OPTS="-m $memory -smp $cores" \
        "$vm_runner"
) >"$qemu_log" 2>&1 &
qemu_pid=$!

ssh_options=(
    -o ConnectTimeout=3
    -o PreferredAuthentications=password
    -o PubkeyAuthentication=no
    -o StrictHostKeyChecking=no
    -o UserKnownHostsFile="$known_hosts"
    -p "$ssh_port"
)

ssh_guest() {
    SSHPASS="${PODMAN_ROOTLESS_QEMU_SSH_PASSWORD:-root}" sshpass -e ssh \
        "${ssh_options[@]}" root@127.0.0.1 "$@"
}

ssh_guest_user() {
    SSHPASS="${PODMAN_ROOTLESS_QEMU_SSH_PASSWORD:-root}" sshpass -e ssh \
        "${ssh_options[@]}" podman@127.0.0.1 "$@"
}

echo "==> waiting for rootless Podman guest"
for attempt in {1..120}; do
    if ! kill -0 "$qemu_pid" 2>/dev/null; then
        echo "QEMU exited before the guest became ready" >&2
        echo "==> complete QEMU log: $qemu_log" >&2
        cat "$qemu_log" >&2 || true
        exit 1
    fi
    if ssh_guest true \
        && ssh_guest_user env HOME=/home/podman XDG_RUNTIME_DIR=/run/user/1000 \
            podman info --format '{{.Host.Security.Rootless}}' | grep -qx true; then
        break
    fi
    if [[ "$attempt" -eq 120 ]]; then
        echo "timed out waiting for rootless Podman" >&2
        echo "==> complete QEMU log: $qemu_log" >&2
        cat "$qemu_log" >&2 || true
        exit 1
    fi
    sleep 2
done

echo "==> copying nixos-dockers into the guest"
ssh_guest rm -rf /workspace/nixos-dockers
ssh_guest mkdir -p /workspace/nixos-dockers
tar -C "$repo_root" -cf - . | ssh_guest tar -xf - -C /workspace/nixos-dockers
ssh_guest chown -R podman:podman /workspace/nixos-dockers

echo "==> running images/podman/tests/common.sh as rootless podman"
ssh_guest_user env \
    NIX_PATH="nixpkgs=/etc/nixpkgs" \
    HOME=/home/podman \
    XDG_RUNTIME_DIR=/run/user/1000 \
    bash -s <<'GUEST_TEST'
set -Eeuo pipefail
cd /workspace/nixos-dockers
test "$(id -u)" = 1000
test "$(id -g)" = 1000
test "$(podman info --format '{{.Host.Security.Rootless}}')" = true
exec bash images/podman/tests/common.sh
GUEST_TEST

echo "==> rootless Podman image test completed successfully"
