{ system ? builtins.currentSystem
, name ? "mise"
}:
let
  sources = import ./npins;
  pkgs = import sources.nixpkgs { inherit system; config.allowUnfree = true; };
  builder = import ../../modules { inherit pkgs sources system; };
  miseRepo = import sources.mise-nixcache { inherit pkgs; };
  miseModules = [
    {
      profiles.mise = {
        enable = true;
        package = miseRepo.mise;
      };
    }
  ];
in
builder.buildImages {
  inherit name;
  modules = miseModules;
}
