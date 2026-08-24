{ sources ? import ./npins
, system ? builtins.currentSystem
, pkgs ? import sources.nixpkgs { inherit system; config.allowUnfree = true; }
, name ? "mise"
}:
let
  builder = import ../../modules { inherit pkgs sources system; };
in
builder.buildImages {
  inherit name;
  modules = [
    {
      profiles.mise.enable = true;
    }
  ];
}

