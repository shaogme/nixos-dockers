{ system ? builtins.currentSystem
, pkgs ? import <nixpkgs> { inherit system; }
, output ? "image"
}:

let
  nixos = import <nixpkgs/nixos> {
    inherit system;
    configuration = {
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
      ];
      boot.kernelModules = [
        "br_netfilter"
        "fuse"
        "overlay"
        "tun"
      ];
      boot.kernel.sysctl = {
        "net.ipv4.ip_forward" = 1;
        "net.ipv6.conf.all.forwarding" = 1;
        # Keep nested rootless containers from being rejected by the guest
        # kernel before Podman gets a chance to create their user namespace.
        "user.max_user_namespaces" = 15000;
      };

      # This VM is the outer runtime for the image under test. dockerCompat
      # deliberately makes the existing image test use this rootless Podman
      # installation without changing the Docker-oriented test itself.
      virtualisation.podman = {
        enable = true;
        dockerCompat = true;
        defaultNetwork.settings.dns_enabled = true;
      };
      virtualisation.containers = {
        containersConf.settings = {
          containers = {
            cgroupns = "private";
          };
          engine = {
            cgroup_manager = "cgroupfs";
            runtime = "crun";
          };
        };
        storage.settings = {
          storage = {
            driver = "overlay";
            graphroot = "/var/lib/containers/storage";
            runroot = "/run/containers/storage";
            rootless_storage_path = "$HOME/.local/share/containers/storage";
          };
          storage.options.overlay = {
            mount_program = "${pkgs.fuse-overlayfs}/bin/fuse-overlayfs";
            mountopt = "nodev,metacopy=on";
          };
        };
      };

      virtualisation.mountHostNixStore = false;
      virtualisation.useNixStoreImage = false;

      # The guest builds images from the checkout. Keep a local nixpkgs path
      # available so the test does not depend on the host's NIX_PATH.
      nix.enable = true;
      nix.nixPath = [ "nixpkgs=/etc/nixpkgs" ];
      nix.settings.allowed-users = [ "root" "podman" ];
      environment.etc."nixpkgs".source = pkgs.path;
      # The test user runs nix-build directly. Give it access to the daemon
      # socket while retaining the normal non-root build permissions.
      systemd.sockets.nix-daemon.socketConfig = {
        SocketGroup = "nixbld";
        SocketMode = "0660";
      };

      environment.systemPackages = with pkgs; [
        bash
        coreutils
        curl
        crun
        fuse-overlayfs
        git
        nix
        openssh
        podman
        qemu-utils
        slirp4netns
        sshpass
      ];

      users.groups.podman.gid = 1000;
      users.users.podman = {
        isNormalUser = true;
        uid = 1000;
        group = "podman";
        extraGroups = [ "nixbld" ];
        home = "/home/podman";
        createHome = true;
        shell = pkgs.bashInteractive;
        linger = true;
        initialHashedPassword =
          "$6$tEI3gQs0Btzt1Chd$se4yg0TbtA7DeNXG.H19YOVHTkc.4INnG5xpn4QX/EmH536zpQBbrR/Wp.BkzgnQdDF3OUvNl.rg7bMO.faLq1";
        subUidRanges = [
          {
            startUid = 100000;
            count = 65536;
          }
        ];
        subGidRanges = [
          {
            startGid = 100000;
            count = 65536;
          }
        ];
      };

      services.openssh = {
        enable = true;
        settings = {
          PermitRootLogin = "yes";
          PasswordAuthentication = true;
        };
      };

      users.mutableUsers = false;
      users.users.root.initialHashedPassword =
        "$6$tEI3gQs0Btzt1Chd$se4yg0TbtA7DeNXG.H19YOVHTkc.4INnG5xpn4QX/EmH536zpQBbrR/Wp.BkzgnQdDF3OUvNl.rg7bMO.faLq1";

      # Rootless Podman needs to pass these devices to the nested engine. The
      # VM has no other users, so making them readable is sufficient here.
      services.udev.extraRules = ''
        KERNEL=="fuse", MODE="0666"
        KERNEL=="tun", MODE="0666"
      '';

      networking.firewall.enable = false;
      virtualisation.sharedDirectories = pkgs.lib.mkForce { };
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
