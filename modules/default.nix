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
  ];

  evalContainer = { modules ? [ ], specialArgs ? { } }:
    lib.evalModules {
      modules = coreModules ++ modules;
      specialArgs = { inherit pkgs sources system; } // specialArgs;
    };

  buildVscodeImage = args:
    (evalContainer args).config.docker.build;
in
{
  inherit coreModules evalContainer buildVscodeImage;
}
