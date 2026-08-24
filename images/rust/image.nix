{ sources ? import ./npins
, system ? builtins.currentSystem
, pkgs ? import sources.nixpkgs { inherit system; config.allowUnfree = true; }
, imageName ? "rust"
}:
let
  builder = import ../../modules { inherit pkgs sources system; };
in
builder.buildImage {
  modules = [
    {
      docker.name = imageName;
      profiles.rust.enable = true;
    }
  ];
}
