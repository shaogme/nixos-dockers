{ config, lib, pkgs, ... }:
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

    environment.systemPackages = with pkgs; [
      npins
    ];
  };
}
