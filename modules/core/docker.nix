{ config, lib, pkgs, ... }:
let
  validVars = lib.filterAttrs (_: value: value != null) config.environment.variables;
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
    );
    extraCommands = config.docker.extraCommands;
    fakeRootCommands = config.docker.fakeRootCommands;
    config = {
      Entrypoint = lib.optionals config.runtime.enable [
        "/usr/bin/container-init"
        "run"
        "--"
      ];
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
      type = lib.types.enum [ "runtime" "builder" ];
      default = "runtime";
      description = "Image role used to select runtime or build-stage defaults.";
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
}
