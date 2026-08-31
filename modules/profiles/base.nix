{ config, lib, pkgs, ... }:
{
  options.profiles.base = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Enable base development tools and runtime libraries for VS Code Remote.";
    };
  };

  config = lib.mkIf config.profiles.base.enable {
    environment.runtimeLibraries = with pkgs; [
      glibc
      stdenv.cc.cc.lib
      zlib
      openssl
      icu
      libsecret
      util-linux
      libxml2
      libuv
      curl
    ];

    environment.systemPackages = with pkgs; [
      # Core Tools & Build Essentials
      bashInteractive
      gcc
      glibc.bin # Contains ldd, required for version checks
      coreutils
      findutils
      file
      gnugrep
      gnused
      gawk
      gnutar
      gzip
      wget
      which
      xz
      cacert

      # Modern CLI & Productivity Utilities
      ripgrep
      fd
      jq
      zip
      unzip
      p7zip
      zstd

      # Network, Git & System Diagnostics
      curl
      iproute2
      iputils
      dnsutils
      procps
      strace
      lsof
      vim
      nano
      ncurses

      # Nix Ecosystem Utilities
      nix
      nix-ld
      direnv
      nix-direnv
    ] ++ lib.optionals config.services.openssh.enable [
      openssh
    ];
  };
}
