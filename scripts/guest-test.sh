#!/usr/bin/env bash
set -Eeuo pipefail

if [[ "${NIXOS_DOCKERS_QEMU_GUEST:-}" != 1 ]]; then
    echo "tests must be dispatched by scripts/qemu-test.sh inside a QEMU guest" >&2
    exit 2
fi

runtime="${NIXOS_DOCKERS_GUEST_RUNTIME:-}"
if [[ "$runtime" != rootful-docker && "$runtime" != rootless-podman ]]; then
    echo "unsupported guest runtime: ${runtime:-<empty>}" >&2
    exit 2
fi

root_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

require_rootful_docker() {
    [[ "$runtime" == rootful-docker ]] || {
        echo "this target requires the rootful Docker guest" >&2
        exit 2
    }
    [[ "$(id -u)" == 0 ]]
    systemctl is-active --quiet docker
    docker info >/dev/null
    [[ "$(stat -fc %T /sys/fs/cgroup)" == cgroup2fs ]]
    [[ -w /sys/fs/cgroup ]]
    [[ -c /dev/fuse ]]

    local cap_eff
    cap_eff="$(awk '/^CapEff:/ { print $2 }' /proc/self/status)"
    (( (16#$cap_eff & (1 << 21)) != 0 )) || {
        echo "rootful Docker guest is missing CAP_SYS_ADMIN" >&2
        exit 1
    }

    docker run --rm --privileged --device /dev/fuse \
        --env NIXOS_DOCKERS_QEMU_REQUIRED=1 \
        docker.io/library/bash:5.2 \
        bash -euc 'test "$(id -u)" = 0; test -w /sys/fs/cgroup; test -c /dev/fuse'
}

require_rootless_podman() {
    [[ "$runtime" == rootless-podman ]] || {
        echo "this target requires the rootless Podman guest" >&2
        exit 2
    }
    [[ "$(id -u)" == 1000 && "$(id -g)" == 1000 ]]
    local info
    info="$(podman info --format '{{.Host.Security.Rootless}}:{{.Store.GraphRoot}}')"
    [[ "$info" == "true:/home/podman/.local/share/containers/storage" ]]
    [[ -w /home/podman/.local/share/containers/storage ]]
    command -v docker >/dev/null
}

ensure_rust_toolchain() {
    export RUSTUP_HOME=/var/lib/rustup
    export CARGO_HOME=/var/lib/cargo
    export PATH="$CARGO_HOME/bin:$PATH"
    if ! rustup toolchain list | awk '{ print $1 }' | grep -Fxq 1.88.0; then
        rustup toolchain install 1.88.0 --profile minimal --component clippy --component rustfmt
    fi
    rustup default 1.88.0
    rustc --version | grep -F 'rustc 1.88.0 '
    cargo --version | grep -F 'cargo 1.88.0 '
}

run_workspace() {
    require_rootful_docker
    ensure_rust_toolchain
    cargo clippy --workspace --all-targets --locked --no-default-features -- -D warnings
    cargo build --locked -p dev-env-provider --bin dev-env-provider-worker
    cargo test --workspace --locked --no-default-features -- --test-threads=1
    cargo install cargo-cop --version 0.3.11 --locked
    cargo cop --all-targets --all-features --workspace
}

run_docker_target() {
    local target="$1"
    require_rootful_docker
    case "$target" in
        mise)
            require_mise_cache
            bash images/mise/tests/docker.sh
            ;;
        npins) bash images/npins/tests/docker.sh ;;
        rust) bash images/rust/tests/docker.sh ;;
        podman) bash images/podman/tests/docker.sh ;;
        *)
            echo "unsupported image target: $target" >&2
            exit 2
            ;;
    esac
}

require_mise_cache() {
    local cache_url="${NIXOS_DOCKERS_MISE_CACHE_URL:-}"
    local public_key="${NIXOS_DOCKERS_MISE_CACHE_PUBLIC_KEY:-}"
    [[ "$cache_url" =~ ^http://10\.0\.2\.2:[0-9]+$ ]] || {
        echo "mise image tests require the forwarded NixCache proxy" >&2
        exit 2
    }
    [[ -n "$public_key" && "${NIX_CONFIG:-}" == *"extra-substituters = $cache_url"* && "${NIX_CONFIG:-}" == *"extra-trusted-public-keys = $public_key"* ]] || {
        echo "mise image tests require NixCache as a trusted Nix substituter" >&2
        exit 2
    }

    echo "==> requiring the mise package from NixCache (local builds disabled)"
    nix-build --no-out-link --option max-jobs 0 -E '
      let
        sources = import ./images/mise/npins;
        system = builtins.currentSystem;
        pkgs = import sources.nixpkgs { inherit system; config.allowUnfree = true; };
        miseRepo = import sources.mise-nixcache { inherit pkgs; };
      in miseRepo.mise
    '
}

run_crate_docker_suite() {
    case "$runtime" in
        rootful-docker) require_rootful_docker ;;
        rootless-podman) require_rootless_podman ;;
        *)
            echo "unsupported crate Docker suite runtime: $runtime" >&2
            exit 2
            ;;
    esac

    local target
    for target in \
        container-init-posix \
        container-init-core \
        container-init-cli \
        dev-env-core \
        dev-env-provider \
        dev-env-shell \
        dev-env-cli; do
        echo "==> crate Docker suite: $target ($runtime)"
        bash "crates/$target/tests/docker.sh"
    done
}

run_rootless_podman_target() {
    [[ "$1" == podman ]] || {
        echo "only the podman target is supported in the rootless guest" >&2
        exit 2
    }
    require_rootless_podman
    bash images/podman/tests/docker.sh
}

case "${1:-}" in
    workspace)
        [[ $# == 1 ]] || exit 2
        run_workspace
        ;;
    docker)
        [[ $# == 2 ]] || exit 2
        run_docker_target "$2"
        ;;
    crates)
        [[ $# == 1 ]] || exit 2
        run_crate_docker_suite
        ;;
    podman)
        [[ $# == 1 ]] || exit 2
        run_rootless_podman_target podman
        ;;
    *)
        echo "usage: $0 workspace | crates | docker <image-target> | podman" >&2
        exit 2
        ;;
esac
