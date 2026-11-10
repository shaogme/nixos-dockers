{ system ? builtins.currentSystem
, pkgs ? import <nixpkgs> { inherit system; }
, output ? "image"
}:

let
  nixos = import <nixpkgs/nixos> {
    inherit system;
    configuration = { config, lib, ... }: {
      nixpkgs.pkgs = pkgs;

      imports = [
        <nixpkgs/nixos/modules/virtualisation/disk-image.nix>
        <nixpkgs/nixos/modules/virtualisation/qemu-vm.nix>
      ];

      image.format = "qcow2";
      image.efiSupport = false;

      boot.loader.grub.device = "/dev/vda";
      boot.loader.timeout = 0;
      boot.kernelParams = [
        "console=ttyS0,115200n8"
        "console=tty0"
        "systemd.unified_cgroup_hierarchy=1"
      ];
      boot.kernelModules = [ "9p" "9pnet_virtio" "br_netfilter" "fuse" "overlay" ];
      boot.kernel.sysctl = {
        "net.ipv4.ip_forward" = 1;
        "net.ipv6.conf.all.forwarding" = 1;
      };

      virtualisation.docker = {
        enable = true;
        enableOnBoot = true;
        storageDriver = "overlay2";
      };
      virtualisation.mountHostNixStore = false;
      virtualisation.useNixStoreImage = false;

      nix.enable = true;
      nix.nixPath = [ "nixpkgs=/etc/nixpkgs" ];
      nix.settings.allowed-users = [ "root" ];
      nix.settings.experimental-features = [ "nix-command" ];
      environment.etc."nixpkgs".source = pkgs.path;

      virtualisation.fileSystems."/mnt/qemu-share" = {
        device = "qemu-share";
        fsType = "9p";
        options = [
          "trans=virtio"
          "version=9p2000.L"
          "msize=16384"
          "x-systemd.requires=modprobe@9pnet_virtio.service"
          "ro"
        ];
      };
      users.allowNoPasswordLogin = true;
      systemd.services.qemu-authorized-key = {
        description = "Install the temporary QEMU launcher SSH key";
        wantedBy = [ "sshd.service" ];
        before = [ "sshd.service" ];
        after = [ "local-fs.target" ];
        serviceConfig.Type = "oneshot";
        path = [ pkgs.coreutils ];
        script = ''
          install -d -m 0700 /root/.ssh
          install -m 0600 /mnt/qemu-share/ssh.pub /root/.ssh/authorized_keys
        '';
      };
      systemd.services.sshd.requires = [ "qemu-authorized-key.service" ];
      systemd.services.sshd.after = [ "qemu-authorized-key.service" ];

      environment.systemPackages = with pkgs; [
        bash
        coreutils
        curl
        docker
        git
        nix
        openssh
        python3
        qemu-utils
        rustup
        stdenv.cc
      ];

      services.openssh = {
        enable = true;
        settings = {
          PermitRootLogin = "prohibit-password";
          PasswordAuthentication = false;
          KbdInteractiveAuthentication = false;
          PubkeyAuthentication = true;
        };
      };

      users.mutableUsers = false;
      networking.firewall.enable = false;
      virtualisation.sharedDirectories = lib.mkForce { };
      virtualisation.graphics = false;
      virtualisation.memorySize = 4096;
      virtualisation.cores = 4;
      virtualisation.diskSize = 32768;

      system.stateVersion = "25.05";
    };
  };
  outputs = {
    image = nixos.config.system.build.image;
    kernel = nixos.config.system.build.kernel;
    initrd = nixos.config.system.build.initialRamdisk;
    toplevel = nixos.config.system.build.toplevel;
    vm = nixos.config.system.build.vm;
  };
in
if output == "metadata" then outputs else outputs.image
