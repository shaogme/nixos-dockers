{ sources ? import ./npins
, system ? builtins.currentSystem
, pkgs ? import sources.nixpkgs { inherit system; config.allowUnfree = true; }
, imageName ? "vscode-npins"
}:
let
  builder = import ../../modules { inherit pkgs sources system; };
in
builder.buildVscodeImage {
  modules = [
    {
      docker.name = imageName;
      profiles.npins.enable = true;
    }
  ];
}
