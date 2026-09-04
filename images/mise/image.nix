{ sources ? import ./npins
, system ? builtins.currentSystem
, pkgs ? import sources.nixpkgs { inherit system; config.allowUnfree = true; }
, name ? "mise"
}:
let
  builder = import ../../modules { inherit pkgs sources system; };
  miseRepo = import sources.mise-nixcache { inherit pkgs; };
in
builder.buildImages {
  inherit name;
  modules = [
    {
      profiles.mise = {
        enable = true;
        package = miseRepo.mise;
      };
    }
  ];
}

