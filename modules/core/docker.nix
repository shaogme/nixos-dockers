{ config, lib, pkgs, ... }:
let
  sshEntrypointSnippet = lib.optionalString config.services.openssh.enable ''
    # SSH Host Keys
    if [ ! -f /etc/ssh/ssh_host_rsa_key ]; then
        ssh-keygen -f /etc/ssh/ssh_host_rsa_key -N "" -t rsa >/dev/null 2>&1
    fi
    if [ ! -f /etc/ssh/ssh_host_ed25519_key ]; then
        ssh-keygen -f /etc/ssh/ssh_host_ed25519_key -N "" -t ed25519 >/dev/null 2>&1
    fi

    # Setup Authorized Keys from /tmp/id_ed25519.pub
    if [ -f "/tmp/id_ed25519.pub" ]; then
        mkdir -p /root/.ssh
        if [ ! -f /root/.ssh/authorized_keys ]; then
            cp /tmp/id_ed25519.pub /root/.ssh/authorized_keys
            chmod 700 /root/.ssh
            chmod 600 /root/.ssh/authorized_keys
        fi
        if [ "$TARGET_USER" != "root" ] && [ -n "$USER_HOME" ]; then
            mkdir -p "$USER_HOME/.ssh"
            if [ ! -f "$USER_HOME/.ssh/authorized_keys" ]; then
                cp /tmp/id_ed25519.pub "$USER_HOME/.ssh/authorized_keys"
                chmod 700 "$USER_HOME/.ssh"
                chmod 600 "$USER_HOME/.ssh/authorized_keys"
            fi
            chown -R "$TARGET_UID:$TARGET_GID" "$USER_HOME/.ssh"
        fi
    fi

    # Ensure SSH Directories
    mkdir -p /var/run/sshd /var/empty/sshd
    chmod 755 /var/empty/sshd

    # Export Environment for SSH Sessions
    mkdir -p /root/.ssh
    chmod 700 /root/.ssh
    env | grep -E "^(PATH|NIX_|CARGO_|RUST_|PKG_CONFIG|LD_)" > /root/.ssh/environment || true
    chmod 600 /root/.ssh/environment
    if [ "$TARGET_USER" != "root" ] && [ -n "$USER_HOME" ]; then
        mkdir -p "$USER_HOME/.ssh"
        cp /root/.ssh/environment "$USER_HOME/.ssh/environment" 2>/dev/null || true
        chmod 600 "$USER_HOME/.ssh/environment" 2>/dev/null || true
        chown -R "$TARGET_UID:$TARGET_GID" "$USER_HOME/.ssh" 2>/dev/null || true
    fi
  '';

  defaultCmd =
    if config.services.openssh.enable then ''
      echo "Starting SSH server..."
      exec /bin/sshd -D -e
    '' else ''
      if [ "$TARGET_USER" != "root" ]; then
          export HOME="$USER_HOME"
          export USER="$TARGET_USER"
          exec ${pkgs.su-exec}/bin/su-exec "$TARGET_USER" /bin/bash -i
      else
          exec /bin/bash -i
      fi
    '';

  generatedEntrypoint = pkgs.writeScriptBin "entrypoint.sh" ''
    #!/usr/bin/env bash
    set -e

    # ==========================================
    # NixOS Development Container Entrypoint
    # ==========================================

    # Fix Permissions for Config Files (Symlink Handling for dynamic user mapping)
    for file in /etc/passwd /etc/group; do
        if [ -f "$file" ] && { [ -L "$file" ] || [ ! -w "$file" ]; }; then
            cp --remove-destination "$(readlink -f "$file")" "$file"
        fi
    done
    chmod 644 /etc/passwd /etc/group 2>/dev/null || true

    # Ensure Metadata Directories & Directory Permissions
    mkdir -p /var/lock /var/tmp /run /workspace /run/containers /var/lib/containers
    chmod 1777 /var/lock /var/tmp /run /run/containers
    chmod 700 /root 2>/dev/null || true

    # ==========================================
    # Adaptive UID/GID Resolution
    # ==========================================
    TARGET_UID=""
    TARGET_GID=""

    # 1. Parse from HOST_UID / HOST_GID environment variables
    # Supports formats:
    #   HOST_UID="1000:1000"
    #   HOST_UID="1000" HOST_GID="1000"
    if [ -n "$HOST_UID" ]; then
        if [[ "$HOST_UID" == *:* ]]; then
            TARGET_UID="''${HOST_UID%%:*}"
            TARGET_GID="''${HOST_UID##*:}"
        else
            TARGET_UID="$HOST_UID"
            TARGET_GID="''${HOST_GID:-$HOST_UID}"
        fi
    elif [ -n "$HOST_GID" ]; then
        TARGET_GID="$HOST_GID"
    fi

    # 2. Runtime user mapping probe from workspace directory if not explicitly set
    WORKSPACE_DIR="''${WORKSPACE:-''${WORKSPACE_DIR:-${config.docker.workingDir}}}"
    if [ -z "$TARGET_UID" ] && [ "${if config.docker.autoUserMapping then "1" else "0"}" = "1" ] && [ "$RUN_AS_ROOT" != "1" ]; then
        if [ -d "$WORKSPACE_DIR" ]; then
            PROBED_UID=$(stat -c '%u' "$WORKSPACE_DIR" 2>/dev/null || echo 0)
            PROBED_GID=$(stat -c '%g' "$WORKSPACE_DIR" 2>/dev/null || echo 0)
            if [ "$PROBED_UID" -gt 0 ] 2>/dev/null; then
                TARGET_UID="$PROBED_UID"
                TARGET_GID="''${TARGET_GID:-$PROBED_GID}"
            fi
        fi
    fi

    # Fallback to 0 (root) if still undetermined or RUN_AS_ROOT=1
    TARGET_UID="''${TARGET_UID:-0}"
    TARGET_GID="''${TARGET_GID:-$TARGET_UID}"

    # ==========================================
    # User & Group Mapping Adjustment
    # ==========================================
    DEFAULT_USER="${config.docker.defaultUser}"
    TARGET_USER="root"
    USER_HOME="/root"

    if [ "$TARGET_UID" -ne 0 ]; then
        # 1. Ensure target GID exists in /etc/group
        EXISTING_GROUP=$(awk -F: -v gid="$TARGET_GID" '$3 == gid {print $1; exit}' /etc/group)
        if [ -z "$EXISTING_GROUP" ]; then
            sed -i "s|^''${DEFAULT_USER}:x:[0-9]*:|''${DEFAULT_USER}:x:''${TARGET_GID}:|" /etc/group
            TARGET_GROUP="''${DEFAULT_USER}"
        else
            TARGET_GROUP="$EXISTING_GROUP"
        fi

        # 2. Ensure target UID exists in /etc/passwd
        EXISTING_USER=$(awk -F: -v uid="$TARGET_UID" '$3 == uid {print $1; exit}' /etc/passwd)
        if [ -n "$EXISTING_USER" ]; then
            TARGET_USER="$EXISTING_USER"
            sed -i "s|^''${TARGET_USER}:x:''${TARGET_UID}:[0-9]*:|''${TARGET_USER}:x:''${TARGET_UID}:''${TARGET_GID}:|" /etc/passwd
        else
            sed -i "s|^''${DEFAULT_USER}:x:[0-9]*:[0-9]*:|''${DEFAULT_USER}:x:''${TARGET_UID}:''${TARGET_GID}:|" /etc/passwd
            TARGET_USER="''${DEFAULT_USER}"
        fi

        # 3. Setup User Home Directory & Nix Defexpr Permissions
        USER_HOME=$(awk -F: -v u="$TARGET_USER" '$1 == u {print $6}' /etc/passwd)
        USER_HOME="''${USER_HOME:-/home/$TARGET_USER}"
        chown "$TARGET_UID:$TARGET_GID" "$USER_HOME" 2>/dev/null || true
        chown -h -R "$TARGET_UID:$TARGET_GID" "$USER_HOME/.nix-defexpr" 2>/dev/null || true

        # 4. Fix Workspace & Nix State Permissions for Non-Root User
        if [ -d /workspace ]; then
            chown "$TARGET_UID:$TARGET_GID" /workspace 2>/dev/null || true
        fi
        if [ -d /nix/var/nix ]; then
            chown -R "$TARGET_UID:$TARGET_GID" /nix/var/nix 2>/dev/null || true
        fi
    fi

    ${sshEntrypointSnippet}

    # Execute Command or Start Default Service
    if [ $# -gt 0 ]; then
        if [ "$1" = "/bin/sshd" ] || [ "$1" = "sshd" ]; then
            exec "$@"
        elif [ "$TARGET_USER" != "root" ]; then
            export HOME="$USER_HOME"
            export USER="$TARGET_USER"
            exec ${pkgs.su-exec}/bin/su-exec "$TARGET_USER" "$@"
        else
            exec "$@"
        fi
    else
        ${defaultCmd}
    fi
  '';

  defaultExposedPorts =
    if config.services.openssh.enable then {
      "22/tcp" = { };
    } else { };

  validVars = lib.filterAttrs (_: v: v != null) config.environment.variables;
  envList = [
    "PATH=/nix/var/nix/profiles/default/bin:/bin:/usr/bin:/usr/local/bin"
  ] ++ (lib.mapAttrsToList (name: value: "${name}=${toString value}") validVars);

  layeredImage = (pkgs.dockerTools.buildLayeredImage {
    name = config.docker.name;
    tag = config.docker.tag;
    includeNixDB = true;
    contents = lib.unique (
      config.environment.systemPackages
      ++ config.environment.runtimeLibraries
      ++ config.docker.extraContents
      ++ [ config.docker.entrypoint ]
    );
    extraCommands = config.docker.extraCommands;
    fakeRootCommands = config.docker.fakeRootCommands;
    config = {
      Cmd = [ "/bin/entrypoint.sh" ];
      WorkingDir = config.docker.workingDir;
      ExposedPorts = config.docker.exposedPorts;
      Env = envList;
    };
  }) // {
    imageVersion = config.docker.version;
    passthru = {
      imageVersion = config.docker.version;
    };
  };
in
{
  options.docker = {
    name = lib.mkOption {
      type = lib.types.str;
      description = "Docker image repository name.";
    };

    tag = lib.mkOption {
      type = lib.types.str;
      default = "latest";
      description = "Docker image tag.";
    };

    version = lib.mkOption {
      type = lib.types.str;
      default = "latest";
      description = "Component version for semantic tagging.";
    };

    workingDir = lib.mkOption {
      type = lib.types.str;
      default = "/workspace";
      description = "Default working directory inside the container.";
    };

    defaultUser = lib.mkOption {
      type = lib.types.str;
      default = config.system.defaultUser;
      description = "Default non-root user name for container operations.";
    };

    autoUserMapping = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Enable adaptive UID/GID mapping in entrypoint.sh based on HOST_UID/HOST_GID or mounted workspace permissions.";
    };

    exposedPorts = lib.mkOption {
      type = lib.types.attrsOf (lib.types.attrsOf lib.types.anything);
      default = defaultExposedPorts;
      description = "Exposed ports in the container config.";
    };

    entrypoint = lib.mkOption {
      type = lib.types.package;
      default = generatedEntrypoint;
      description = "Entrypoint package placed in /bin.";
    };

    extraContents = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = [ ];
      description = "Additional packages to include directly in image contents.";
    };

    extraCommands = lib.mkOption {
      type = lib.types.lines;
      default = "";
      description = "Shell commands executed during layer construction.";
    };

    fakeRootCommands = lib.mkOption {
      type = lib.types.lines;
      default = "";
      description = "Shell commands executed inside fakeroot during layer construction.";
    };

    build = lib.mkOption {
      type = lib.types.package;
      default = layeredImage;
      description = "The resulting Docker layered image derivation.";
    };
  };
}

