{ config, lib, pkgs, ... }:
let
  containersPolicy = pkgs.writeTextDir "etc/containers/policy.json" ''
    {
      "default": [
        {
          "type": "insecureAcceptAnything"
        }
      ]
    }
  '';

  containersRegistries = pkgs.writeTextDir "etc/containers/registries.conf" ''
    unqualified-search-registries = ["docker.io", "quay.io"]
  '';

  containersStorage = pkgs.writeTextDir "etc/containers/storage.conf" ''
    [storage]
    driver = "overlay"
    runroot = "/run/containers/storage"
    graphroot = "/var/lib/containers/storage"

    [storage.options]
    pull_options = { enable_partial_images = "true", use_hard_links = "false", ostree_repos = "" }

    [storage.options.overlay]
    mount_program = "/usr/bin/fuse-overlayfs"
    mountopt = "nodev,metacopy=on"
  '';

  containersConfig = pkgs.writeTextDir "etc/containers/containers.conf" ''
    [containers]
    cgroups = "enabled"
    cgroupns = "private"
    userns = "host"

    [engine]
    cgroup_manager = "cgroupfs"
    runtime = "crun"
    events_logger = "file"
    network_backend = "netavark"
    network_cmd_path = "/usr/bin/netavark"

    [network]
    default_rootless_network_cmd = "slirp4netns"
  '';

  passwd = pkgs.writeTextDir "etc/passwd" ''
    root:x:0:0:root:/root:/bin/sh
  '';

  group = pkgs.writeTextDir "etc/group" ''
    root:x:0:
  '';

  entrypoint = pkgs.writeTextFile {
    name = "podman-engine-entrypoint";
    destination = "/usr/local/bin/podman-engine-entrypoint";
    executable = true;
    text = ''
      #!${pkgs.busybox}/bin/sh
      set -eu

      socket_dir=/run/podman
      socket_path="$socket_dir/podman.sock"
      socket_gid="''${PODMAN_SOCKET_GID:-1000}"
      socket_mode="''${PODMAN_SOCKET_MODE:-0660}"

      mkdir -p "$socket_dir" /run/containers/storage /var/lib/containers/storage
      chgrp "$socket_gid" "$socket_dir" 2>/dev/null || true
      chmod 0770 "$socket_dir"

      if [ "$#" -eq 0 ]; then
        set -- /usr/bin/podman system service --time=0 "unix://$socket_path"
      fi

      "$@" &
      service_pid=$!
      cleanup() {
        kill "$service_pid" 2>/dev/null || true
        wait "$service_pid" 2>/dev/null || true
      }
      trap cleanup INT TERM EXIT

      attempts=0
      while [ ! -S "$socket_path" ] && [ "$attempts" -lt 100 ]; do
        attempts=$((attempts + 1))
        sleep 0.1
      done

      if [ ! -S "$socket_path" ]; then
        echo "podman engine failed to create $socket_path" >&2
        exit 1
      fi
      chmod "$socket_mode" "$socket_path"
      chgrp "$socket_gid" "$socket_path" 2>/dev/null || true

      wait "$service_pid"
    '';
  };

  packages = with pkgs; [
    podman
    crun
    conmon
    fuse-overlayfs
    netavark
    aardvark-dns
    slirp4netns
    iptables
    busybox
  ];
in
{
  options.profiles.podman = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Enable the minimal rootful Podman engine service image.";
    };
  };

  config = lib.mkIf config.profiles.podman.enable {
    docker.includeNixDB = false;
    docker.environmentPath = "/usr/bin:/bin";
    docker.user = "0:0";
    docker.version = lib.mkDefault pkgs.podman.version;
    docker.extraContents = [
      containersConfig
      containersPolicy
      containersRegistries
      containersStorage
      entrypoint
      passwd
      group
    ];
    docker.extraCommands = ''
      mkdir -p bin usr/bin usr/local/bin etc/containers tmp var/tmp workspace root run/podman run/containers/storage var/lib/containers/storage
      chmod 1777 tmp var/tmp workspace
      ln -sf ${pkgs.busybox}/bin/busybox bin/sh
      ln -sf ${pkgs.busybox}/bin/busybox usr/bin/sh
      ln -sf ${pkgs.podman}/bin/podman usr/bin/podman
      ln -sf /usr/bin/podman bin/podman
      ln -sf ${pkgs.crun}/bin/crun usr/bin/crun
      ln -sf ${pkgs.conmon}/bin/conmon usr/bin/conmon
      ln -sf ${pkgs.fuse-overlayfs}/bin/fuse-overlayfs usr/bin/fuse-overlayfs
      ln -sf ${pkgs.netavark}/bin/netavark usr/bin/netavark
      ln -sf ${pkgs.aardvark-dns}/bin/aardvark-dns usr/bin/aardvark-dns
      ln -sf ${pkgs.slirp4netns}/bin/slirp4netns usr/bin/slirp4netns
      ln -sf ${pkgs.iptables}/bin/iptables usr/bin/iptables
      ln -sf ${pkgs.iptables}/bin/iptables-nft usr/bin/iptables-nft
    '';
    environment.systemPackages = packages;
    environment.variables = {
      CONTAINERS_CONF = "/etc/containers/containers.conf";
      CONTAINERS_STORAGE_CONF = "/etc/containers/storage.conf";
      CONTAINERS_REGISTRIES_CONF = "/etc/containers/registries.conf";
      PODMAN_SOCKET_GID = "1000";
      PODMAN_SOCKET_MODE = "0660";
    };
    runtime.enable = false;
    profiles.base.enable = false;
    system.enable = false;
    environment.enable = false;
    services.openssh.enable = false;
  };
}
