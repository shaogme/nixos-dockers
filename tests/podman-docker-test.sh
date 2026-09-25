#!/usr/bin/env bash
set -Eeuo pipefail

# Run images/podman/tests/docker.sh inside a Docker daemon hosted by a QEMU
# NixOS guest. This preserves the nested cgroup topology from CI and checks
# that the engine receives a writable cgroup2 hierarchy for crun.

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/.." && pwd)"
image_nix="$script_dir/docker-image.nix"
ssh_port="${PODMAN_QEMU_SSH_PORT:-22230}"
memory="${PODMAN_QEMU_MEMORY:-4096}"
cores="${PODMAN_QEMU_CORES:-4}"
work_dir="${PODMAN_QEMU_WORK_DIR:-${TMPDIR:-/tmp}/nixos-dockers-podman-qemu-test}"
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

for command in nix-build qemu-img ssh sshpass tar; do
    require_command "$command"
done

mkdir -p "$work_dir"

cleanup() {
    local status=$?
    if [[ -n "$qemu_pid" ]] && kill -0 "$qemu_pid" 2>/dev/null; then
        kill "$qemu_pid" 2>/dev/null || true
        wait "$qemu_pid" 2>/dev/null || true
    fi
    if [[ "$status" -ne 0 && -f "$qemu_log" ]]; then
        echo "==> QEMU log ($qemu_log)" >&2
        tail -n 160 "$qemu_log" >&2 || true
    fi
    exit "$status"
}
trap cleanup EXIT

echo "==> building the Docker NixOS qcow2 image"
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

echo "==> starting Docker NixOS VM"
(
    cd "$work_dir"
    QEMU_NET_OPTS="hostfwd=tcp:127.0.0.1:$ssh_port-:22" \
        NIX_DISK_IMAGE="$vm_disk" \
        QEMU_OPTS="-m $memory -smp $cores" \
        "$vm_runner"
) >"$qemu_log" 2>&1 &
qemu_pid=$!

ssh_guest() {
    SSHPASS="${PODMAN_QEMU_SSH_PASSWORD:-root}" sshpass -e ssh \
        -q \
        -o ConnectTimeout=3 \
        -o PreferredAuthentications=password \
        -o PubkeyAuthentication=no \
        -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile="$known_hosts" \
        -p "$ssh_port" \
        root@127.0.0.1 \
        "$@"
}

echo "==> waiting for SSH and Docker in the guest"
for attempt in {1..120}; do
    if ! kill -0 "$qemu_pid" 2>/dev/null; then
        echo "QEMU exited before the guest became ready" >&2
        exit 1
    fi
    if ssh_guest true 2>/dev/null \
        && ssh_guest systemctl is-active --quiet docker \
        && ssh_guest docker info >/dev/null 2>&1; then
        break
    fi
    if [[ "$attempt" -eq 120 ]]; then
        echo "timed out waiting for the guest Docker daemon" >&2
        exit 1
    fi
    sleep 2
done

echo "==> copying nixos-dockers into the guest"
ssh_guest mkdir -p /workspace/nixos-dockers
tar -C "$repo_root" -cf - . | ssh_guest tar -xf - -C /workspace/nixos-dockers

echo "==> running images/podman/tests/docker.sh in the guest"
ssh_guest env NIX_PATH="nixpkgs=/etc/nixpkgs" bash -s <<'GUEST_TEST'
set -Eeuo pipefail
cd /workspace/nixos-dockers
exec bash images/podman/tests/docker.sh
GUEST_TEST

echo "==> Podman Docker test completed successfully"
