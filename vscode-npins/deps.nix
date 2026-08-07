{ pkgs }:
let
  # 1. System Compatibility Libraries (nix-ld)
  # Commonly required by VS Code Server, Copilot, etc.
  runtimeLibs = with pkgs; [
    stdenv.cc.cc.lib
    zlib
    openssl
    icu
    libsecret
    glib
    libkrb5
    util-linux
  ];

  # 2. Development Tools
  devTools = with pkgs; [
    # Core
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
    
    # Nix Utilities
    nix
    nix-ld
    direnv
    nix-direnv

    # npins
    npins
  ];
in
{
  inherit runtimeLibs devTools;
  all = devTools ++ runtimeLibs;
}
