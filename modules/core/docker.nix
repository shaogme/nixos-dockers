{ config, lib, pkgs, ... }:
let
  defaultEntrypoint = pkgs.writeScriptBin "entrypoint.sh" (builtins.readFile ./entrypoint.sh);

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
      default = {
        "22/tcp" = { };
      };
      description = "Exposed ports in the container config.";
    };

    entrypoint = lib.mkOption {
      type = lib.types.package;
      default = defaultEntrypoint;
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
