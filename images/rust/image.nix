{ system ? builtins.currentSystem
, name ? "rust"
}:
let
  sources = import ./npins;
  pkgs = import sources.nixpkgs { inherit system; config.allowUnfree = true; };
  builder = import ../../modules { inherit pkgs sources system; };
in
builder.buildImages {
  inherit name;
  modules = [
    {
      profiles.rust.enable = true;
    }
  ];
}

