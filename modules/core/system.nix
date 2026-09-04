{ config, lib, pkgs, ... }:
let
  passwd = pkgs.writeTextDir "etc/passwd" ''
    root:x:0:0:System Administrator:/root:/bin/bash
    ${config.system.defaultUser}:x:${toString config.system.defaultUid}:${toString config.system.defaultGid}:Developer:/home/${config.system.defaultUser}:/bin/bash
    ${lib.optionalString config.services.openssh.enable "sshd:x:74:74:Privilege-separated SSH:/var/empty/sshd:/sbin/nologin\n"}
  '';

  group = pkgs.writeTextDir "etc/group" ''
    root:x:0:
    ${config.system.defaultUser}:x:${toString config.system.defaultGid}:
    ${lib.optionalString config.services.openssh.enable "sshd:x:74:\n"}
    nixbld:x:30000:
    wheel:x:998:${config.system.defaultUser}
  '';


  sshdConfig = pkgs.writeTextDir "etc/ssh/sshd_config" ''
    PermitRootLogin yes
    PasswordAuthentication yes
    PubkeyAuthentication yes
    UsePAM yes
    Port 22
    HostKey /etc/ssh/ssh_host_rsa_key
    HostKey /etc/ssh/ssh_host_ed25519_key
    Subsystem sftp internal-sftp
    PermitUserEnvironment yes
    PermitEmptyPasswords yes
    PidFile /var/run/sshd.pid
  '';

  pamSshd = pkgs.writeTextDir "etc/pam.d/sshd" ''
    auth       sufficient   pam_permit.so
    account    sufficient   pam_permit.so
    password   sufficient   pam_permit.so
    session    sufficient   pam_permit.so
  '';

  nsswitchConf = pkgs.writeTextDir "etc/nsswitch.conf" ''
    passwd:    files
    group:     files
    shadow:    files
    hosts:     files dns
  '';

  nixConf = pkgs.writeTextDir "etc/nix/nix.conf" ''
    build-users-group =
    experimental-features = nix-command flakes
    filter-syscalls = false
    trusted-users = root ${config.system.defaultUser}
  '';

  etcEnvironment = pkgs.writeTextDir "etc/environment" ''
    NIX_LD=${config.environment.nixLd}
    NIX_LD_LIBRARY_PATH=${config.environment.nixLdLibPath}:/usr/lib:/usr/lib64
    LD_LIBRARY_PATH=${config.environment.nixLdLibPath}:/usr/lib:/usr/lib64
    NIX_SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
    NIX_PATH=nixpkgs=${pkgs.path}
    PATH=/bin:/usr/bin:/usr/local/bin
  '';

  systemConfigFiles = [
    passwd
    group
    nsswitchConf
    nixConf
    etcEnvironment
    pkgs.iana-etc
    pkgs.dockerTools.caCertificates
  ] ++ lib.optionals config.services.openssh.enable [
    sshdConfig
    pamSshd
  ];

  shadowContent =
    if config.services.openssh.enable then ''
      root::19733:0:99999:7:::
      ${config.system.defaultUser}::19733:0:99999:7:::
      sshd:*:19733:0:99999:7:::
    '' else ''
      root::19733:0:99999:7:::
      ${config.system.defaultUser}::19733:0:99999:7:::
    '';

  sshExtraCommands = lib.optionalString config.services.openssh.enable ''
    mkdir -p root/.ssh
    chmod 700 root/.ssh

    mkdir -p run var/run/sshd var/empty/sshd
    chmod 755 var/empty/sshd

    if [ -f etc/pam.d/sshd ]; then
      cp etc/pam.d/sshd etc/pam.d/other
    fi
  '';
in
{
  options.services.openssh = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Enable OpenSSH daemon, configuration, host keys, and port exposure.";
    };
  };

  options.system = {
    defaultUser = lib.mkOption {
      type = lib.types.str;
      default = "dev";
      description = "Default non-root user name for container operations.";
    };

    defaultUid = lib.mkOption {
      type = lib.types.int;
      default = 1000;
      description = "Default UID for the non-root user.";
    };

    defaultGid = lib.mkOption {
      type = lib.types.int;
      default = 1000;
      description = "Default GID for the non-root user.";
    };

    configFiles = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = systemConfigFiles;
      description = "Base system configuration files placed in image contents.";
    };
  };

  config = {
    docker.extraContents = config.system.configFiles;
    docker.extraCommands = ''
      # 1. Base System Directories
      mkdir -p tmp workspace
      chmod 1777 tmp
      chmod 777 workspace
      
      mkdir -p run var/lock var/tmp
      chmod 1777 var/lock
      chmod 1777 var/tmp
      
      ${sshExtraCommands}

      # 2. Generate /etc/shadow and /etc/sudoers
      cat > etc/shadow <<EOF
      ${shadowContent}EOF
      chmod 600 etc/shadow

      cat > etc/sudoers <<EOF
      root ALL=(ALL:ALL) ALL
      %wheel ALL=(ALL:ALL) NOPASSWD: ALL
      ${config.system.defaultUser} ALL=(ALL:ALL) NOPASSWD: ALL
      EOF
      chmod 440 etc/sudoers

      # 3. Non-Root User Setup & Defexpr
      mkdir -p home/${config.system.defaultUser}/.nix-defexpr
      ln -sf ${pkgs.path} home/${config.system.defaultUser}/.nix-defexpr/nixpkgs
      mkdir -p root
      chmod 700 root
      # Backward compatibility symlink
      ln -sf /workspace root/workspace

      # 4. Multi-user / Non-root Nix profile directories
      mkdir -p nix/var/nix/profiles/per-user nix/var/nix/gcroots/per-user
      chmod 1777 nix/var/nix/profiles/per-user nix/var/nix/gcroots/per-user

      # 5. FHS Compatibility (Required for VS Code Server, Mise unpatched binaries, etc.)
      mkdir -p lib lib64 usr/lib64 usr/lib usr/bin usr/lib/x86_64-linux-gnu usr/lib/aarch64-linux-gnu

      # Link Dynamic Linker (ld-linux) to nix-ld for unpatched FHS binary support (AMD64 & ARM64)
      ln -sf ${pkgs.nix-ld}/libexec/nix-ld lib64/ld-linux-x86-64.so.2
      ln -sf ${pkgs.nix-ld}/libexec/nix-ld lib/ld-linux-x86-64.so.2
      ln -sf ${pkgs.nix-ld}/libexec/nix-ld usr/lib64/ld-linux-x86-64.so.2
      ln -sf ${pkgs.nix-ld}/libexec/nix-ld usr/lib/ld-linux-x86-64.so.2
      ln -sf ${pkgs.nix-ld}/libexec/nix-ld lib64/ld-linux-aarch64.so.1
      ln -sf ${pkgs.nix-ld}/libexec/nix-ld lib/ld-linux-aarch64.so.1
      ln -sf ${pkgs.nix-ld}/libexec/nix-ld usr/lib64/ld-linux-aarch64.so.1
      ln -sf ${pkgs.nix-ld}/libexec/nix-ld usr/lib/ld-linux-aarch64.so.1
      
      # Link Core Libraries (libstdc++, libgcc_s)
      for lib in libstdc++.so.6 libgcc_s.so.1; do
        cp -L ${pkgs.stdenv.cc.cc.lib}/lib/$lib usr/lib/$lib
        cp -L ${pkgs.stdenv.cc.cc.lib}/lib/$lib usr/lib64/$lib
        ln -sf /usr/lib/$lib lib64/$lib
        ln -sf /usr/lib/$lib lib/$lib
        ln -sf /usr/lib/$lib usr/lib/x86_64-linux-gnu/$lib
        ln -sf /usr/lib/$lib usr/lib/aarch64-linux-gnu/$lib
      done
      
      # 6. Bash Wrapper
      rm -f bin/bash
      cp ${config.environment.bashWrapper} bin/bash
      chmod +x bin/bash

      # 7. Setup /usr/bin/env & Gosu
      ln -sf ${pkgs.coreutils}/bin/env usr/bin/env
      ln -sf ${pkgs.gosu}/bin/gosu usr/bin/gosu
      ln -sf ${pkgs.gosu}/bin/gosu bin/gosu
      
      # 8. Common Tools Symlinks
      ln -sf ${pkgs.procps}/bin/pgrep usr/bin/pgrep
      ln -sf ${pkgs.procps}/bin/pkill usr/bin/pkill
      ln -sf ${pkgs.procps}/bin/ps usr/bin/ps
      ln -sf ${pkgs.coreutils}/bin/uname usr/bin/uname
      ln -sf ${pkgs.coreutils}/bin/dirname usr/bin/dirname
      ln -sf ${pkgs.coreutils}/bin/readlink usr/bin/readlink
      ln -sf ${pkgs.coreutils}/bin/wc usr/bin/wc
      ln -sf ${pkgs.ripgrep}/bin/rg usr/bin/rg
      ln -sf ${pkgs.fd}/bin/fd usr/bin/fd
      ln -sf ${pkgs.jq}/bin/jq usr/bin/jq
      ln -sf ${pkgs.git}/bin/git usr/bin/git
      ln -sf ${pkgs.gh}/bin/gh usr/bin/gh

      # 9. Setup Nix Search Path & Defexpr for root
      mkdir -p root/.nix-defexpr
      ln -sf ${pkgs.path} root/.nix-defexpr/nixpkgs
    '';

    docker.fakeRootCommands = ''
      target_home="home/${config.system.defaultUser}"
      if [ ! -d "$target_home" ] && [ -d "/$target_home" ]; then
        target_home="/$target_home"
      fi
      if [ -d "$target_home" ]; then
        chown -R ${toString config.system.defaultUid}:${toString config.system.defaultGid} "$target_home"
      fi
    '';
  };
}

