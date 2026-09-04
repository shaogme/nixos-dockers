{ config, lib, pkgs, ... }:
let
  nixLdLibPath = lib.makeLibraryPath config.environment.runtimeLibraries;
  nixLd = lib.fileContents "${pkgs.stdenv.cc}/nix-support/dynamic-linker";

  # Format non-null environment variables for export in bash wrapper
  validVars = lib.filterAttrs (_: v: v != null) config.environment.variables;
  exportLines = lib.concatStringsSep "\n" (
    lib.mapAttrsToList (name: value: ''export ${name}="${toString value}"'') validVars
  );

  bashWrapper = pkgs.writeScript "bash-wrapper" ''
    #!${pkgs.bashInteractive}/bin/bash
    export NIX_LD_LIBRARY_PATH="${nixLdLibPath}:/usr/lib:/usr/lib64"
    export NIX_LD="${nixLd}"
    export LD_LIBRARY_PATH="${nixLdLibPath}:/usr/lib:/usr/lib64"
    export PATH=$PATH:/usr/local/bin:/usr/bin:/bin
    if [ -d "/nix/var/nix/profiles/default/bin" ]; then
      export PATH="/nix/var/nix/profiles/default/bin:$PATH"
    fi
    if [ -n "$HOME" ] && [ -d "$HOME/.nix-profile/bin" ]; then
      export PATH="$HOME/.nix-profile/bin:$PATH"
    fi
    if [ -z "$USER" ]; then
      export USER="$(id -un 2>/dev/null || echo "dev")"
    fi
    ${exportLines}
    if [ -f /etc/bash.bashrc ]; then
      . /etc/bash.bashrc
    fi
    exec ${pkgs.bashInteractive}/bin/bash "$@"
  '';
in
{
  options.environment = {
    systemPackages = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = [ ];
      description = "List of packages installed in the Docker image.";
    };

    runtimeLibraries = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = [ ];
      description = "List of dynamic libraries exposed to nix-ld and LD_LIBRARY_PATH for FHS binary compatibility.";
    };

    variables = lib.mkOption {
      type = lib.types.attrsOf (lib.types.nullOr (lib.types.oneOf [ lib.types.str lib.types.path lib.types.package ]));
      default = { };
      description = "Environment variables set in the Docker container and bash sessions.";
    };

    nixLdLibPath = lib.mkOption {
      type = lib.types.str;
      internal = true;
      default = nixLdLibPath;
      description = "Calculated library path for nix-ld and dynamic linker fallback.";
    };

    nixLd = lib.mkOption {
      type = lib.types.str;
      internal = true;
      default = nixLd;
      description = "Dynamic linker path.";
    };

    bashWrapper = lib.mkOption {
      type = lib.types.package;
      internal = true;
      default = bashWrapper;
      description = "Wrapper script to preserve environment variables during SSH and interactive sessions.";
    };
  };

  config = {
    environment.systemPackages = [
      pkgs.su-exec
      pkgs.shadow
      pkgs.fuse-overlayfs
      pkgs.slirp4netns
      pkgs.iptables
    ];
    environment.variables = {
      NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
      NIX_PATH = "nixpkgs=${pkgs.path}";
      NIX_PROFILE = "/nix/var/nix/profiles/default";
      NIX_LD = nixLd;
      NIX_LD_LIBRARY_PATH = "${nixLdLibPath}:/usr/lib:/usr/lib64";
      LD_LIBRARY_PATH = "${nixLdLibPath}:/usr/lib:/usr/lib64";
    };
  };
}
