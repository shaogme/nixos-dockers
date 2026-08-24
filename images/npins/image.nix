{ sources ? import ./npins
, system ? builtins.currentSystem
, pkgs ? import sources.nixpkgs { inherit system; config.allowUnfree = true; }
, imageName ? "npins"
}:
let
  builder = import ../../modules { inherit pkgs sources system; };
in
builder.buildImage {
  modules = [
    {
      docker.name = imageName;
      profiles.npins.enable = true;
    }
  ];
}
