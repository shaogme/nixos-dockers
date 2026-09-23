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
          services.openssh.enable = lib.mkDefault false;
        }
      ] ++ modules;
    }).config.docker.build;

  buildVscodeImage = { modules ? [ ], specialArgs ? { } }:
    (evalContainer {
      inherit specialArgs;
      modules = [
        {
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
          services.openssh.enable = false;
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
  inherit coreModules evalContainer buildImage buildVscodeImage buildBuilderImage buildImages;
}
