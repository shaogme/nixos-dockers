{ config, lib, pkgs, ... }:
let
  npins = pkgs.runCommand "npins-${pkgs.npins.version}" {
    nativeBuildInputs = [ pkgs.makeWrapper ];
  } ''
    mkdir -p $out/bin
    makeWrapper ${pkgs.npins}/bin/npins $out/bin/npins \
      --prefix LD_LIBRARY_PATH : ${pkgs.aws-lc}/lib
  '';
in
{
  options.profiles.npins = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Enable npins dependency management utility.";
    };
  };

  config = lib.mkIf config.profiles.npins.enable {
    docker.version = lib.mkDefault pkgs.npins.version;

    environment.systemPackages = [ npins ];
  };
}
