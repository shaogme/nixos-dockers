{ pkgs
, sources ? null
, system ? pkgs.system
}:
let
  lib = pkgs.lib;

  coreModules = [
    ./core/docker.nix
    ./core/system.nix
    ./core/environment.nix
    ./core/runtime.nix
    ./profiles/base.nix
    ./profiles/rust.nix
    ./profiles/npins.nix
    ./profiles/mise.nix
    ./profiles/podman.nix
  ];

  evalContainer = { modules ? [ ], specialArgs ? { } }:
    lib.evalModules {
      modules = coreModules ++ modules;
      specialArgs = { inherit pkgs sources system; } // specialArgs;
    };

  buildImage = { modules ? [ ], specialArgs ? { } }:
    (evalContainer {
      inherit specialArgs;
      modules = [
        {
          docker.role = "runtime";
          docker.includeNixDB = true;
          docker.environmentPath = "/nix/var/nix/profiles/default/bin:/bin:/usr/bin:/usr/local/bin";
          docker.entrypoint = [ "/usr/bin/container-init" "run" "--" ];
          runtime.enable = true;
          profiles.base.enable = true;
          system.enable = true;
          environment.enable = true;
          services.openssh.enable = lib.mkDefault false;
        }
      ] ++ modules;
    }).config.docker.build;

  buildVscodeImage = { modules ? [ ], specialArgs ? { } }:
    (evalContainer {
      inherit specialArgs;
      modules = [
        {
          docker.role = "runtime";
          docker.includeNixDB = true;
          docker.environmentPath = "/nix/var/nix/profiles/default/bin:/bin:/usr/bin:/usr/local/bin";
          docker.entrypoint = [ "/usr/bin/container-init" "run" "--" ];
          runtime.enable = true;
          profiles.base.enable = true;
          system.enable = true;
          environment.enable = true;
          services.openssh.enable = lib.mkDefault true;
        }
      ] ++ modules;
    }).config.docker.build;

  buildBuilderImage = { name, modules ? [ ], specialArgs ? { } }:
    (evalContainer {
      inherit specialArgs;
      modules = [
        {
          docker.name = lib.mkDefault "${name}-builder";
          docker.role = "builder";
          docker.includeNixDB = true;
          docker.environmentPath = "/nix/var/nix/profiles/default/bin:/bin:/usr/bin:/usr/local/bin";
          docker.entrypoint = [ ];
          profiles.base.enable = true;
          system.enable = true;
          environment.enable = true;
          runtime.enable = false;
          services.openssh.enable = false;
        }
      ] ++ modules;
    }).config.docker.build;

  buildEngineImage = { name ? "podman", modules ? [ ], specialArgs ? { } }:
    (evalContainer {
      inherit specialArgs;
      modules = [
        {
          docker.name = lib.mkDefault name;
          docker.role = "engine";
          docker.includeNixDB = false;
          docker.environmentPath = "/usr/bin:/bin";
          docker.entrypoint = [ "/usr/local/bin/podman-engine-entrypoint" ];
          runtime.enable = false;
          profiles.base.enable = false;
          system.enable = false;
          environment.enable = false;
          services.openssh.enable = false;
          profiles.podman.enable = true;
        }
      ] ++ modules;
    }).config.docker.build;

  buildImages = { name, modules ? [ ], specialArgs ? { } }: {
    ${name} = buildImage {
      inherit specialArgs;
      modules = [
        {
          docker.name = lib.mkDefault name;
        }
      ] ++ modules;
    };
    "vscode-${name}" = buildVscodeImage {
      inherit specialArgs;
      modules = [
        {
          docker.name = lib.mkDefault "vscode-${name}";
        }
      ] ++ modules;
    };
    "${name}-builder" = buildBuilderImage {
      inherit name specialArgs;
      modules = modules;
    };
  };
in
{
  inherit coreModules evalContainer buildImage buildVscodeImage buildBuilderImage buildEngineImage buildImages;
}
