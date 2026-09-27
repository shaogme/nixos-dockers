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
    # fuse-overlayfs provides the overlay mount for the unprivileged engine;
    # the outer runtime only needs the tested namespaced capabilities for
    # crun and rootless UID/GID mapping, never privileged mode.
    driver = "overlay"
    runroot = "/run/user/1000/containers"
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
    [engine]
    cgroup_manager = "cgroupfs"
    runtime = "crun"
    events_logger = "file"
    network_backend = "netavark"
    # network_cmd_path is the rootless slirp4netns helper path; netavark is
    # selected independently through network_backend above.
    network_cmd_path = "/usr/bin/slirp4netns"

    [network]
    default_rootless_network_cmd = "slirp4netns"
  '';

  passwd = pkgs.writeTextDir "etc/passwd" ''
    root:x:0:0:root:/root:/bin/sh
    podman:x:1000:1000:Podman Engine:/home/podman:/bin/sh
  '';

  group = pkgs.writeTextDir "etc/group" ''
    root:x:0:
    podman:x:1000:
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
      map_dir=/run/user/1000/containers
      uid_map_file="$map_dir/subuid"
      gid_map_file="$map_dir/subgid"

      # A rootless outer runtime exposes its subordinate IDs in the current
      # user namespace, not in the host namespace.  Translate the first
      # column of uid_map/gid_map into the ranges that an inner newuidmap can
      # actually write, while leaving the image defaults intact for a rootful
      # outer runtime.
      write_nested_map() {
        source_map="$1"
        destination="$2"
        current_id="$3"
        owner="$4"

        if ${pkgs.busybox}/bin/awk '
          $1 == 0 && $2 == 0 && $3 >= 4294967295 { identity = 1 }
          END { exit identity ? 0 : 1 }
        ' "$source_map"; then
          ${pkgs.busybox}/bin/cp "/etc/$(${pkgs.busybox}/bin/basename "$destination")" "$destination"
          return 0
        fi

        ${pkgs.busybox}/bin/awk -v owner="$owner" -v current="$current_id" '
          function emit(start, count, end, left, right) {
            end = start + count
            # UID/GID zero is the outer namespace root and is intentionally
            # not delegated as a subordinate ID.
            if (start < 1) {
              start = 1
            }
            if (start < end && current >= start && current < end) {
              left = current - start
              right = end - (current + 1)
              if (left > 0) {
                printf "%s:%d:%d\n", owner, start, left
              }
              if (right > 0) {
                printf "%s:%d:%d\n", owner, current + 1, right
              }
            } else if (start < end) {
              printf "%s:%d:%d\n", owner, start, end - start
            }
          }
          /^[[:space:]]*[0-9]+[[:space:]]+[0-9]+[[:space:]]+[0-9]+[[:space:]]*$/ {
            emit($1, $3)
          }
        ' "$source_map" > "$destination"

        if [ ! -s "$destination" ]; then
          echo "podman engine could not derive subordinate IDs from $source_map" >&2
          return 1
        fi
      }

      mkdir -p "$socket_dir" /run/user/1000/containers /var/lib/containers/storage
      current_user="$(${pkgs.busybox}/bin/id -un)"
      current_group="$(${pkgs.busybox}/bin/id -gn)"
      write_nested_map /proc/self/uid_map "$uid_map_file" "$(id -u)" "$current_user"
      write_nested_map /proc/self/gid_map "$gid_map_file" "$(id -g)" "$current_group"
      if ! ${pkgs.busybox}/bin/mount --bind "$uid_map_file" /etc/subuid; then
        echo "podman engine could not mount dynamic /etc/subuid" >&2
        exit 1
      fi
      if ! ${pkgs.busybox}/bin/mount --bind "$gid_map_file" /etc/subgid; then
        echo "podman engine could not mount dynamic /etc/subgid" >&2
        exit 1
      fi
      echo "podman engine uid_map:" >&2
      cat /proc/self/uid_map >&2
      echo "podman engine gid_map:" >&2
      cat /proc/self/gid_map >&2
      echo "podman engine subuid:" >&2
      cat /etc/subuid >&2
      echo "podman engine subgid:" >&2
      cat /etc/subgid >&2
      # A rootless process can only change the group to one it owns. Keep the
      # socket group configurable so clients can opt into their shared group.
      if ! chgrp "$socket_gid" "$socket_dir"; then
        echo "podman engine could not set socket directory group to $socket_gid" >&2
      fi
      # The socket volume may be initialized as root-owned by Docker. Its
      # image directory is intentionally writable, so the engine can create
      # the socket without needing to chmod/chown the volume mount itself.

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
      if ! chgrp "$socket_gid" "$socket_path"; then
        echo "podman engine could not set socket group to $socket_gid" >&2
      fi

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
      description = "Enable the minimal rootless Podman engine service image.";
    };
  };

  config = lib.mkIf config.profiles.podman.enable {
    docker.includeNixDB = false;
    docker.environmentPath = "/usr/bin:/bin";
    # The engine must not inherit /workspace as its current directory. The
    # Compose service bind-mounts that path for workloads, and a rootless
    # outer runtime may not grant the engine UID traversal permission there.
    docker.workingDir = "/";
    docker.version = lib.mkDefault pkgs.podman.version;
    docker.extraContents = [
      # The engine is built without profiles.base/system, so include the trust
      # store explicitly. Registry clients must validate TLS inside the image.
      pkgs.dockerTools.caCertificates
      containersConfig
      containersPolicy
      containersRegistries
      containersStorage
      entrypoint
      passwd
      group
    ];
    docker.extraCommands = ''
      mkdir -p bin usr/bin usr/local/bin etc/containers tmp var/tmp workspace root home/podman run/podman run/user/1000/containers var/lib/containers/storage
      chmod 1777 tmp var/tmp workspace
      # Keep the image layer root-owned so the outer runtime can map its root
      # user directly to the host user; application directories stay writable
      # for runtimes that apply their own rootless identity policy.
      chmod 0777 home/podman run/podman run/user/1000/containers var/lib/containers/storage
      # Rootless workloads need a subordinate range for image layer ownership.
      # The outer runtime maps the image's root user to its regular user.
      printf 'root:100000:65536\n' > etc/subuid
      printf 'root:100000:65536\n' > etc/subgid
      chmod 0644 etc/subuid etc/subgid
      ln -sf ${pkgs.busybox}/bin/busybox bin/sh
      ln -sf ${pkgs.busybox}/bin/busybox usr/bin/sh
      ln -sf ${pkgs.busybox}/bin/busybox bin/mount
      ln -sf ${pkgs.busybox}/bin/busybox usr/bin/mount
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
      cp -L ${pkgs.shadow}/bin/newuidmap usr/bin/newuidmap
      cp -L ${pkgs.shadow}/bin/newgidmap usr/bin/newgidmap
    '';
    docker.fakeRootCommands = ''
      chmod 0777 home/podman run/podman run/user/1000 var/lib/containers
      chmod 4755 usr/bin/newuidmap usr/bin/newgidmap
    '';
    environment.systemPackages = packages;
    environment.variables = {
      CONTAINERS_CONF = "/etc/containers/containers.conf";
      CONTAINERS_STORAGE_CONF = "/etc/containers/storage.conf";
      CONTAINERS_REGISTRIES_CONF = "/etc/containers/registries.conf";
      SSL_CERT_FILE = "/etc/ssl/certs/ca-bundle.crt";
      NIX_SSL_CERT_FILE = "/etc/ssl/certs/ca-bundle.crt";
      # Keep these paths usable when an outer runtime applies its own identity
      # policy to an image without a User field; rootless Podman derives the
      # real identity from the process UID and namespace map.
      HOME = "/home/podman";
      USER = "podman";
      LOGNAME = "podman";
      XDG_RUNTIME_DIR = "/run/user/1000";
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
