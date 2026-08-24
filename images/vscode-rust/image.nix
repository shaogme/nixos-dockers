{ sources ? import ./npins
, system ? builtins.currentSystem
, pkgs ? import sources.nixpkgs { inherit system; config.allowUnfree = true; }
, imageName ? "vscode-rust"
}:
let
  builder = import ../../modules { inherit pkgs sources system; };
in
builder.buildVscodeImage {
  modules = [
    {
      docker.name = imageName;
      profiles.rust.enable = true;
    }
  ];
}
