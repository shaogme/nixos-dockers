{ config, lib, pkgs, ... }:
{
  options.profiles.rust = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Enable Rust toolchain, debuggers, and ecosystem tools.";
    };
  };

  config = lib.mkIf config.profiles.rust.enable {
    docker.version = lib.mkDefault pkgs.rustc.version;

    environment.systemPackages = with pkgs; [
      # Debugging
      gdb
      lldb

      # Rust Ecosystem & Toolchain
      cargo
      rustc
      rust-analyzer
      clippy
      rustfmt
      cargo-nextest
      cargo-expand
      cargo-audit
      cargo-deny
      cargo-watch
      bacon
      pkg-config
      openssl.dev
    ];

    environment.variables = {
      RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
    };
  };
}
