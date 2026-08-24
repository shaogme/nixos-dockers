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
      stdenv.cc.cc.lib
      zlib
      openssl
      icu
      libsecret
      glib
      libkrb5
      util-linux
    ];

    environment.systemPackages = with pkgs; [
      # Core Tools & Build Essentials
      gcc
      glibc
      glibc.bin # Contains ldd, required for version checks
      coreutils
      findutils
      gnugrep
      gnused
      gawk
      gnutar
      gzip
      wget
      which
      xz
      cacert
      bashInteractive

      # Modern CLI & Productivity Utilities
      ripgrep
      fd
      eza
      bat
      jq
      yq-go
      htop
      bottom
      tree
      less
      zip
      unzip
      p7zip
      zstd

      # Network, Git & System Diagnostics
      curl
      git
      git-lfs
      gh
      openssh
      iproute2
      net-tools
      iputils
      dnsutils
      tcpdump
      nmap
      procps
      psmisc
      strace
      lsof
      vim
      shadow

      # Nix Ecosystem Utilities
      nix
      nix-ld
      direnv
      nix-direnv
    ];
  };
}
