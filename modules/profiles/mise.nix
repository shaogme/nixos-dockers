{ config, lib, pkgs, sources ? null, ... }:
let
  misePackage =
    if sources != null && sources ? mise then
      pkgs.callPackage (sources.mise + "/default.nix") { }
    else
      throw "profiles.mise requires `sources.mise` to be provided. Fallback to pkgs.mise is not permitted.";
in
{
  options.profiles.mise = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Enable mise (dev tools, env vars, and task runner).";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = misePackage;
      description = "The mise package derivation to use.";
    };
  };

  config = lib.mkIf config.profiles.mise.enable {
    docker.version = lib.mkDefault config.profiles.mise.package.version;

    environment.systemPackages = [
      config.profiles.mise.package
    ];
  };
}
