{
  system ? builtins.currentSystem,
  pkgs ? import <nixpkgs> { inherit system; },
  output ? "image",
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
        "systemd.unified_cgroup_hierarchy=1"
      ];
      boot.kernelModules = [
        "br_netfilter"
        "fuse"
        "overlay"
        "9p"
        "9pnet_virtio"
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
        storage.rootless.settings = {
          storage = {
            driver = "overlay";
            graphroot = "/home/podman/.local/share/containers/storage";
            runroot = "/run/user/1000/containers";
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
      nix.settings.allowed-users = [
        "root"
        "podman"
      ];
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
        util-linux
        slirp4netns
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
          PermitRootLogin = "prohibit-password";
          PasswordAuthentication = false;
          KbdInteractiveAuthentication = false;
          PubkeyAuthentication = true;
        };
      };

      users.mutableUsers = false;
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
