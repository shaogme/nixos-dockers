#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
    echo "usage: $0 workspace | crates --runtime rootful-docker|rootless-podman | <image-target> | podman --runtime ... | nixos-reaper" >&2
}

if [[ $# -lt 1 ]]; then
    usage
    exit 2
fi

target="$1"
shift
runtime=rootful-docker
if [[ $# -gt 0 ]]; then
    [[ $# == 2 && "$1" == --runtime ]] || { usage; exit 2; }
    runtime="$2"
    shift 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root_dir="$(cd -- "$script_dir/.." && pwd)"
artifact_root="${QEMU_TEST_ARTIFACTS:-${TMPDIR:-/tmp}/nixos-dockers-qemu-artifacts}"

require_command() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "required command not found: $1" >&2
        exit 127
    }
}

for command in nix-build nix-instantiate nix-prefetch-url qemu-img ssh ssh-keygen tar python3; do
    require_command "$command"
done

resolve_pinned_nixpkgs() {
    local pin_dir="$1"
    local nixpkgs_path

    if [[ -n "${NPINS_OVERRIDE_nixpkgs:-}" ]]; then
        nixpkgs_path="$(nix-instantiate --eval --raw -E "let sources = import $pin_dir; in sources.nixpkgs.outPath")"
    else
        local source_json="$pin_dir/sources.json"
        local pin_url pin_hash prefetched
        pin_url="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["pins"]["nixpkgs"]["url"])' "$source_json")"
        pin_hash="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["pins"]["nixpkgs"]["hash"])' "$source_json")"
        prefetched="$(nix-prefetch-url --unpack --name source --print-path "$pin_url" "$pin_hash")"
        nixpkgs_path="${prefetched##*$'\n'}"
    fi

    [[ -d "$nixpkgs_path" ]] || {
        echo "pinned nixpkgs source is missing after prefetch: $nixpkgs_path" >&2
        return 1
    }
    printf '%s' "$nixpkgs_path"
}

if [[ "$target" == nixos-reaper ]]; then
    [[ $# == 0 && "$runtime" == rootful-docker ]] || { usage; exit 2; }
    mkdir -p "$artifact_root"
    log_file="$artifact_root/nixos-reaper.log"
    pin_dir="$root_dir/images/rust/npins"
    nixpkgs_path="$(resolve_pinned_nixpkgs "$pin_dir")"
    export NIX_PATH="nixpkgs=$nixpkgs_path"
    set +e
    nix-build "$root_dir/tests/container-init-reaper-vm.nix" -A driver -o "$artifact_root/nixos-reaper-driver" 2>&1 | tee "$log_file"
    build_status=${PIPESTATUS[0]}
    if [[ $build_status == 0 ]]; then
        "$artifact_root/nixos-reaper-driver/bin/nixos-test-driver" 2>&1 | tee -a "$log_file"
        test_status=${PIPESTATUS[0]}
    else
        test_status=$build_status
    fi
    set -e
    exit "$test_status"
fi

case "$target" in
    workspace)
        [[ $# == 0 && "$runtime" == rootful-docker ]] || { usage; exit 2; }
        vm_kind=rootful-docker
        dispatcher_args=(workspace)
        ;;
    crates)
        [[ $# == 0 ]] || { usage; exit 2; }
        case "$runtime" in
            rootful-docker) vm_kind=rootful-docker ;;
            rootless-podman) vm_kind=rootless-podman ;;
            *) usage; exit 2 ;;
        esac
        dispatcher_args=(crates)
        ;;
    mise|npins|rust)
        [[ $# == 0 && "$runtime" == rootful-docker ]] || { usage; exit 2; }
        vm_kind=rootful-docker
        dispatcher_args=(docker "$target")
        ;;
    podman)
        [[ $# == 0 ]] || { usage; exit 2; }
        case "$runtime" in
            rootful-docker) vm_kind=rootful-docker; dispatcher_args=(docker podman) ;;
            rootless-podman) vm_kind=rootless-podman; dispatcher_args=(podman) ;;
            *) usage; exit 2 ;;
        esac
        ;;
    *)
        usage
        exit 2
        ;;
esac

case "$runtime" in
    rootful-docker|rootless-podman) ;;
    *) usage; exit 2 ;;
esac

pin_dir="$root_dir/images/rust/npins"
if [[ "$target" == mise || "$target" == npins || "$target" == rust || "$target" == podman ]]; then
    pin_dir="$root_dir/images/$target/npins"
fi
nixpkgs_path="$(resolve_pinned_nixpkgs "$pin_dir")"
export NIX_PATH="nixpkgs=$nixpkgs_path"

mkdir -p "$artifact_root"
run_dir="$(mktemp -d "$artifact_root/${target}-${runtime}.XXXXXX")"
share_dir="$(mktemp -d /tmp/nixos-dockers-qemu-share.XXXXXX)"
qemu_log="$run_dir/qemu.log"
guest_log="$run_dir/guest.log"
ssh_key="$run_dir/ssh-key"
known_hosts="$run_dir/known_hosts"
image_link="$run_dir/image"
vm_link="$run_dir/vm"
disk="$run_dir/vm.qcow2"
image_path=""
vm_runner=""
ssh_port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
qemu_pid=""
nixcache_relay_pid=""
nixcache_relay_port=""

ssh_guest() {
    ssh -q \
        -i "$ssh_key" \
        -o BatchMode=yes \
        -o ConnectTimeout=3 \
        -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile="$known_hosts" \
        -p "$ssh_port" \
        "root@127.0.0.1" "$@"
}

ssh_guest_shell() {
    local remote_command="$1"
    shift

    local argument quoted
    for argument in "$@"; do
        printf -v quoted '%q' "$argument"
        remote_command+=" $quoted"
    done

    ssh_guest "$remote_command"
}

cleanup() {
    local result=$?
    if [[ $result != 0 ]]; then
        echo "==> collecting guest diagnostics" >&2
        if [[ -n "$qemu_pid" ]] && kill -0 "$qemu_pid" 2>/dev/null; then
            ssh_guest 'systemctl --failed; journalctl -b --no-pager -n 300; docker info 2>&1 || true; podman info 2>&1 || true; cat /proc/cgroups; mount | grep cgroup || true' \
                >>"$guest_log" 2>&1 || true
        fi
        echo "==> guest output: $guest_log" >&2
        [[ ! -f "$guest_log" ]] || cat "$guest_log" >&2
        echo "==> QEMU output: $qemu_log" >&2
        [[ ! -f "$qemu_log" ]] || cat "$qemu_log" >&2
    fi
    if [[ -n "$qemu_pid" ]] && kill -0 "$qemu_pid" 2>/dev/null; then
        kill "$qemu_pid" 2>/dev/null || true
        wait "$qemu_pid" 2>/dev/null || true
    fi
    if [[ -n "$nixcache_relay_pid" ]] && kill -0 "$nixcache_relay_pid" 2>/dev/null; then
        kill "$nixcache_relay_pid" 2>/dev/null || true
        wait "$nixcache_relay_pid" 2>/dev/null || true
    fi
    rm -f -- "$disk" "$image_link" "$vm_link" "$run_dir/toplevel" "$known_hosts"
    rm -f -- "$ssh_key" "$ssh_key.pub"
    rm -f -- "$share_dir/ssh.pub"
    rmdir "$share_dir" 2>/dev/null || true
    exit "$result"
}
trap cleanup EXIT

ssh-keygen -q -t ed25519 -N "" -f "$ssh_key"
cp -- "$ssh_key.pub" "$share_dir/ssh.pub"
chmod 0444 "$share_dir/ssh.pub"

if [[ "$target" == mise ]]; then
    nixcache_proxy_port="${QEMU_TEST_NIXCACHE_PROXY_PORT:-}"
    nixcache_public_key="${QEMU_TEST_NIXCACHE_PUBLIC_KEY:-}"
    [[ "$nixcache_proxy_port" =~ ^[0-9]+$ ]] || {
        echo "mise tests require QEMU_TEST_NIXCACHE_PROXY_PORT for the host NixCache proxy" >&2
        exit 2
    }
    [[ -n "$nixcache_public_key" ]] || {
        echo "mise tests require QEMU_TEST_NIXCACHE_PUBLIC_KEY" >&2
        exit 2
    }
    python3 - "$nixcache_proxy_port" <<'PY'
import http.client
import sys

connection = http.client.HTTPConnection("127.0.0.1", int(sys.argv[1]), timeout=5)
connection.request("GET", "/nix-cache-info")
response = connection.getresponse()
if response.status != 200 or b"StoreDir: /nix/store" not in response.read():
    raise SystemExit("host NixCache proxy did not return a valid /nix-cache-info response")
PY
    nixcache_relay_port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("0.0.0.0", 0)); print(s.getsockname()[1]); s.close()')"
    python3 - "$nixcache_relay_port" "$nixcache_proxy_port" >"$run_dir/nixcache-relay.log" 2>&1 <<'PY' &
import socket
import socketserver
import sys
import threading

relay_port = int(sys.argv[1])
proxy_port = int(sys.argv[2])


class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        try:
            upstream = socket.create_connection(("127.0.0.1", proxy_port), timeout=10)
        except OSError:
            return

        def forward(source, destination):
            try:
                while True:
                    data = source.recv(65536)
                    if not data:
                        break
                    destination.sendall(data)
            except OSError:
                pass
            try:
                destination.shutdown(socket.SHUT_WR)
            except OSError:
                pass

        client_to_proxy = threading.Thread(target=forward, args=(self.request, upstream))
        proxy_to_client = threading.Thread(target=forward, args=(upstream, self.request))
        client_to_proxy.start()
        proxy_to_client.start()
        client_to_proxy.join()
        proxy_to_client.join()
        upstream.close()


class Server(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True
    daemon_threads = True


with Server(("0.0.0.0", relay_port), Handler) as server:
    server.serve_forever()
PY
    nixcache_relay_pid=$!
    for attempt in $(seq 1 30); do
        if python3 - "$nixcache_relay_port" 2>/dev/null <<'PY'
import socket
import sys

with socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=1):
    pass
PY
        then
            break
        fi
        if [[ "$attempt" == 30 ]] || ! kill -0 "$nixcache_relay_pid" 2>/dev/null; then
            echo "failed to start the host-to-guest NixCache relay" >&2
            exit 1
        fi
        sleep 1
    done
fi

vm_nix="$root_dir/tests/vms/$vm_kind.nix"
echo "==> evaluating and building $vm_kind system"
nix-instantiate --eval --strict "$vm_nix" --argstr output metadata -A toplevel.drvPath >/dev/null
nix-build "$vm_nix" --argstr output metadata -A toplevel -o "$run_dir/toplevel" >/dev/null
echo "==> building $vm_kind qcow2 image"
nix-build "$vm_nix" -o "$image_link"
image_path="$(readlink -f "$image_link")"
if [[ -d "$image_path" ]]; then
    image_path="$(find "$image_path" -maxdepth 1 -type f -name '*.qcow2' -print -quit)"
fi
[[ -n "$image_path" && -f "$image_path" ]] || {
    echo "could not find a qcow2 image in $image_link" >&2
    exit 1
}
echo "==> building $vm_kind QEMU runner"
nix-build "$vm_nix" --argstr output metadata -A vm -o "$vm_link"
qemu-img create -f qcow2 -F qcow2 -b "$image_path" "$disk" >/dev/null
vm_runner="$(readlink -f "$vm_link")/bin/run-nixos-vm"
[[ -x "$vm_runner" ]] || { echo "NixOS VM runner is missing" >&2; exit 1; }

echo "==> starting $vm_kind on localhost:$ssh_port"
(
    cd "$run_dir"
    QEMU_NET_OPTS="hostfwd=tcp:127.0.0.1:$ssh_port-:22" \
        NIX_DISK_IMAGE="$disk" \
        QEMU_OPTS="-m 4096 -smp 4 -virtfs local,path=$share_dir,mount_tag=qemu-share,security_model=none,readonly=on" \
        "$vm_runner"
) >"$qemu_log" 2>&1 &
qemu_pid=$!

echo "==> waiting for SSH and $runtime"
for attempt in $(seq 1 180); do
    if ! kill -0 "$qemu_pid" 2>/dev/null; then
        echo "QEMU exited before the guest became ready" >&2
        exit 1
    fi
    if [[ "$runtime" == rootful-docker ]]; then
        if ssh_guest 'systemctl is-active --quiet docker && docker info >/dev/null 2>&1' 2>/dev/null; then
            break
        fi
    else
        if ssh_guest 'cd /home/podman && runuser -u podman -- env HOME=/home/podman XDG_RUNTIME_DIR=/run/user/1000 podman info --format "{{.Host.Security.Rootless}}:{{.Store.GraphRoot}}" 2>/dev/null | grep -Fxq "true:/home/podman/.local/share/containers/storage"' 2>/dev/null; then
            break
        fi
    fi
    if [[ "$attempt" == 180 ]]; then
        echo "timed out waiting for guest runtime" >&2
        exit 1
    fi
    sleep 2
done

echo "==> copying the current checkout, including local changes"
ssh_guest 'rm -rf /workspace/nixos-dockers && mkdir -p /workspace/nixos-dockers'
tar -C "$root_dir" --exclude=.git --exclude=target --exclude=result -cf - . \
    | ssh_guest 'tar -xf - -C /workspace/nixos-dockers'

if [[ "$runtime" == rootless-podman ]]; then
    ssh_guest 'chown -R podman:podman /workspace/nixos-dockers'
    echo "==> running $target in the rootless Podman guest"
    set +e
    ssh_guest_shell 'cd /workspace/nixos-dockers && runuser -u podman -- env XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus systemd-run --user --scope --property=Delegate=yes -- env' \
        NIXOS_DOCKERS_QEMU_GUEST=1 \
        NIXOS_DOCKERS_GUEST_RUNTIME=rootless-podman \
        HOME=/home/podman \
        XDG_RUNTIME_DIR=/run/user/1000 \
        NIX_PATH=nixpkgs=/etc/nixpkgs \
        bash /workspace/nixos-dockers/scripts/guest-test.sh \
        "${dispatcher_args[@]}" 2>&1 | tee "$guest_log"
    status=${PIPESTATUS[0]}
    set -e
else
    echo "==> running $target in the rootful Docker guest"
    set +e
    guest_env=(
        NIXOS_DOCKERS_QEMU_GUEST=1
        NIXOS_DOCKERS_GUEST_RUNTIME=rootful-docker
        NIX_PATH=nixpkgs=/etc/nixpkgs
    )
    if [[ "$target" == mise ]]; then
        mise_cache_url="http://10.0.2.2:$nixcache_relay_port"
        mise_cache_config="extra-substituters = $mise_cache_url
extra-trusted-substituters = $mise_cache_url
extra-trusted-public-keys = $nixcache_public_key"
        guest_env+=(
            "NIX_CONFIG=$mise_cache_config"
            "NIXOS_DOCKERS_MISE_CACHE_URL=$mise_cache_url"
            "NIXOS_DOCKERS_MISE_CACHE_PUBLIC_KEY=$nixcache_public_key"
        )
    fi
    ssh_guest_shell env "${guest_env[@]}" \
        bash /workspace/nixos-dockers/scripts/guest-test.sh \
        "${dispatcher_args[@]}" 2>&1 | tee "$guest_log"
    status=${PIPESTATUS[0]}
    set -e
fi

if [[ $status != 0 ]]; then
    exit "$status"
fi
echo "QEMU test passed: $target ($runtime)"
