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
in
{
  inherit coreModules evalContainer buildImage buildVscodeImage;
}
