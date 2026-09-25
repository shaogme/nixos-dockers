{ config, lib, pkgs, ... }:
let
  validVars = lib.filterAttrs (_: value: value != null) config.environment.variables;
  envList = [
    "PATH=${config.docker.environmentPath}"
  ] ++ (lib.mapAttrsToList (name: value: "${name}=${toString value}") validVars);

  layeredImage = (pkgs.dockerTools.buildLayeredImage {
    name = config.docker.name;
    tag = config.docker.tag;
    includeNixDB = config.docker.includeNixDB;
    # Keep derived Docker images below overlayfs' lower-directory limit.
    maxLayers = 64;
    contents = lib.unique (
      config.environment.systemPackages
      ++ config.environment.runtimeLibraries
      ++ config.docker.extraContents
    );
    extraCommands = config.docker.extraCommands;
    fakeRootCommands = config.docker.fakeRootCommands;
    config = {
      Entrypoint = config.docker.entrypoint;
      # An SSH image is a service image by default. The container-init
      # handoff still materializes the environment before starting sshd.
      Cmd = lib.optionals config.services.openssh.enable [
        "/bin/sshd"
        "-D"
        "-e"
      ];
      WorkingDir = config.docker.workingDir;
      ExposedPorts = config.docker.exposedPorts;
      Env = envList;
    } // lib.optionalAttrs (config.docker.user != null) {
      User = config.docker.user;
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
    role = lib.mkOption {
      type = lib.types.enum [ "disabled" "runtime" "builder" "engine" ];
      default = "disabled";
      description = "Image role used to select runtime, builder, or Podman engine defaults.";
    };

    includeNixDB = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Include the Nix database in the image layer.";
    };

    environmentPath = lib.mkOption {
      type = lib.types.str;
      default = "/bin:/usr/bin:/usr/local/bin";
      description = "PATH baked into the image metadata.";
    };

    entrypoint = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Entrypoint command baked into the image metadata.";
    };

    user = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Optional UID/GID used as the default Docker image user.";
    };

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

    gitSafeDirectories = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ config.docker.workingDir ];
      description = "List of directories to mark as Git safe directories in /etc/gitconfig.";
    };

    defaultUser = lib.mkOption {
      type = lib.types.str;
      default = config.system.defaultUser;
      description = "Default non-root user name for container operations.";
    };

    autoUserMapping = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Enable adaptive UID/GID mapping based on runtime bootstrap inputs or the mounted workspace.";
    };

    exposedPorts = lib.mkOption {
      type = lib.types.attrsOf (lib.types.attrsOf lib.types.anything);
      default = lib.optionalAttrs config.services.openssh.enable { "22/tcp" = { }; };
      description = "Exposed ports in the container config.";
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

  # Engine images are infrastructure artifacts. Keep the role self-contained
  # even when a caller forgets to repeat the profile defaults from the builder.
  config = lib.mkIf (config.docker.role == "engine") {
    docker.includeNixDB = false;
    runtime.enable = false;
    profiles.base.enable = false;
    system.enable = false;
    environment.enable = false;
    services.openssh.enable = false;
  };
}
