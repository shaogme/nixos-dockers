{ sources ? import ./npins
, system ? builtins.currentSystem
, pkgs ? import sources.nixpkgs { inherit system; config.allowUnfree = true; }
, name ? "podman"
}:
let
  builder = import ../../modules { inherit pkgs sources system; };
  engine = builder.buildEngineImage {
    inherit name;
    modules = [
      {
        profiles.podman.enable = true;
      }
    ];
  };
in
{
  role = "engine";
  outputs = [ name ];
  meta = {
    role = "engine";
    outputs = [ name ];
  };
  ${name} = engine;
}
