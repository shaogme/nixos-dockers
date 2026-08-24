{ config, lib, pkgs, ... }:
let
  passwd = pkgs.writeTextDir "etc/passwd" ''
    root:x:0:0:System Administrator:/root:/bin/bash
    ${lib.optionalString config.services.openssh.enable "sshd:x:74:74:Privilege-separated SSH:/var/empty/sshd:/sbin/nologin\n"}
  '';

  group = pkgs.writeTextDir "etc/group" ''
    root:x:0:
    ${lib.optionalString config.services.openssh.enable "sshd:x:74:\n"}
    nixbld:x:30000:
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
  '';

  systemConfigFiles = [
    passwd
    group
    nsswitchConf
    nixConf
    pkgs.iana-etc
    pkgs.dockerTools.caCertificates
  ] ++ lib.optionals config.services.openssh.enable [
    sshdConfig
    pamSshd
  ];

  shadowContent =
    if config.services.openssh.enable then ''
      root::19733:0:99999:7:::
      sshd:*:19733:0:99999:7:::
    '' else ''
      root::19733:0:99999:7:::
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
      mkdir -p tmp
      chmod 1777 tmp
      
      mkdir -p run var/lock var/tmp
      chmod 1777 var/lock
      chmod 1777 var/tmp
      
      ${sshExtraCommands}

      # 2. Generate /etc/shadow
      cat > etc/shadow <<EOF
      ${shadowContent}EOF
      chmod 600 etc/shadow
      
      # 3. FHS Compatibility (Required for VS Code Server, etc.)
      mkdir -p lib64 usr/lib64 usr/lib usr/bin usr/lib/x86_64-linux-gnu

      # Link Dynamic Linker (ld-linux)
      ln -sf ${pkgs.glibc}/lib/ld-linux-x86-64.so.2 lib64/ld-linux-x86-64.so.2
      
      # Link Core Libraries (libstdc++, libgcc_s)
      for lib in libstdc++.so.6 libgcc_s.so.1; do
        cp -L ${pkgs.stdenv.cc.cc.lib}/lib/$lib usr/lib/$lib
        cp -L ${pkgs.stdenv.cc.cc.lib}/lib/$lib usr/lib64/$lib
        ln -sf /usr/lib/$lib lib64/$lib
        ln -sf /usr/lib/$lib usr/lib/x86_64-linux-gnu/$lib
      done
      
      # 4. Bash Wrapper
      rm -f bin/bash
      cp ${config.environment.bashWrapper} bin/bash
      chmod +x bin/bash

      # 5. Setup /usr/bin/env
      ln -sf ${pkgs.coreutils}/bin/env usr/bin/env
      
      # 6. Common Tools Symlinks
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

      # 7. Setup Nix Search Path & Defexpr
      mkdir -p root/.nix-defexpr
      ln -sf ${pkgs.path} root/.nix-defexpr/nixpkgs
    '';
  };
}

