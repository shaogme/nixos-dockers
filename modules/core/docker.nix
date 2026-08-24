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
    fi

    # Ensure SSH Directories
    mkdir -p /var/run/sshd /var/empty/sshd
    chmod 755 /var/empty/sshd

    # Export Environment for SSH Sessions
    env | grep -E "^(PATH|NIX_|CARGO_|RUST_|PKG_CONFIG|LD_)" > /root/.ssh/environment || true
    chmod 600 /root/.ssh/environment
  '';

  defaultCmd =
    if config.services.openssh.enable then ''
      echo "Starting SSH server..."
      exec /bin/sshd -D -e
    '' else ''
      exec /bin/bash -i
    '';

  generatedEntrypoint = pkgs.writeScriptBin "entrypoint.sh" ''
    #!/usr/bin/env bash
    set -e

    # ==========================================
    # NixOS Development Container Entrypoint
    # ==========================================

    # Fix Permissions for Config Files (Symlink Handling)
    for file in /etc/passwd /etc/group; do
        if [ -L "$file" ] || [ ! -w "$file" ]; then
            cp --remove-destination "$(readlink -f "$file")" "$file"
            chmod 644 "$file"
        fi
    done

    # Shadow is created via extraCommands so it should be a file, but ensuring permissions is safe.
    if [ -L "/etc/shadow" ]; then
        cp --remove-destination "$(readlink -f /etc/shadow)" /etc/shadow
    fi
    chmod 600 /etc/shadow

    # Ensure Metadata Directories
    mkdir -p /var/lock /var/tmp /run
    chmod 1777 /var/lock /var/tmp

    ${sshEntrypointSnippet}

    # Execute Command or Start Default Service
    if [ $# -gt 0 ]; then
        exec "$@"
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
    "NIX_LD_LIBRARY_PATH=${config.environment.nixLdLibPath}:/usr/lib:/usr/lib64"
    "NIX_LD=${config.environment.nixLd}"
    "LD_LIBRARY_PATH=${config.environment.nixLdLibPath}:/usr/lib:/usr/lib64"
    "PATH=/bin:/usr/bin:/usr/local/bin"
  ] ++ (lib.mapAttrsToList (name: value: "${name}=${toString value}") validVars);

  layeredImage = (pkgs.dockerTools.buildLayeredImage {
    name = config.docker.name;
    tag = config.docker.tag;
    includeNixDB = true;
    contents = config.environment.systemPackages
      ++ config.environment.runtimeLibraries
      ++ config.docker.extraContents
      ++ [ config.docker.entrypoint ];
    extraCommands = config.docker.extraCommands;
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
      default = "/root/workspace";
      description = "Default working directory inside the container.";
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
      description = "Shell commands executed during layer construction in fakeroot.";
    };

    build = lib.mkOption {
      type = lib.types.package;
      default = layeredImage;
      description = "The resulting Docker layered image derivation.";
    };
  };
}

