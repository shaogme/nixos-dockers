{ system ? builtins.currentSystem
, name ? "npins"
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
      profiles.npins.enable = true;
    }
  ];
}

