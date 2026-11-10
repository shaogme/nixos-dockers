let
  system = builtins.currentSystem;
  sources = import ../images/rust/npins;
  pkgs = import sources.nixpkgs { inherit system; };
  containerInit = pkgs.rustPlatform.buildRustPackage {
    pname = "container-init";
    version = "0.1.0";
    src = ../.;
    cargoLock.lockFile = ../Cargo.lock;
    cargoBuildFlags = [ "--package" "container-init-cli" ];
    doCheck = false;
    installPhase = ''
      install -Dm755 target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/container-init $out/bin/container-init
    '';
  };
  handoffScript = pkgs.writeText "container-init-reaper-handoff.sh" ''
    set -eu
    mkdir -p /run/container-init-reaper
    : > /run/container-init-reaper/pids
    while :; do
      /bin/sh -c 'sleep 2 & echo $! >> /run/container-init-reaper/pids' _ &
      sleep 0.02
    done
  '';
in
pkgs.testers.nixosTest {
  name = "container-init-reaper";

  nodes.machine = { lib, ... }: {
    environment.systemPackages = [ pkgs.python3 ];
    environment.etc."container-init/profiles.d/reaper.toml".text = ''
      schema = 1
      id = "reaper-vm"

      [bootstrap]
      workspace_root = "/workspace"

      [bootstrap.identity]
      default_user = "root"
      default_uid = 0
      default_gid = 0
      auto_mapping = false

      [bootstrap.handoff]
      runtime = "/bin/sh"
      exec_prefix = ["-c"]
      initial_exec_prefix = ["-c"]
      shell_prefix = ["-c", "true"]
    '';

    systemd.services.container-init-reaper = {
      description = "container-init adopted-child reaping fixture";
      wantedBy = [ "multi-user.target" ];
      preStart = "mkdir -p /workspace";
      serviceConfig.Type = "simple";
      script = ''
        exec ${containerInit}/bin/container-init \
          --profiles-dir /etc/container-init/profiles.d \
          --profile reaper-vm \
          --workspace /workspace \
          --backend-endpoint vm.container-init-reaper \
          run -- ${lib.escapeShellArg (builtins.readFile handoffScript)}
      '';
    };
  };

  testScript = ''
    machine.wait_for_unit("container-init-reaper.service")
    machine.wait_until_succeeds("test -s /run/container-init-reaper/pids")
    machine.succeed(r"""
      backend_pid=$(systemctl show --property MainPID --value container-init-reaper.service)
      python3 - "$backend_pid" <<'PY'
    import pathlib
    import sys
    import time

    backend_pid = int(sys.argv[1])
    pid_file = pathlib.Path("/run/container-init-reaper/pids")
    children_file = pathlib.Path(f"/proc/{backend_pid}/task/{backend_pid}/children")
    deadline = time.monotonic() + 3
    adopted = False

    def stat(pid):
        fields = pathlib.Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return fields[0], int(fields[1])

    while time.monotonic() < deadline:
        for line in pid_file.read_text().splitlines():
            pid = int(line)
            try:
                state, parent = stat(pid)
            except FileNotFoundError:
                continue
            assert state != "Z", f"adopted process {pid} is a zombie"
            adopted |= parent == backend_pid
        for token in children_file.read_text().split():
            pid = int(token)
            try:
                state, _ = stat(pid)
            except FileNotFoundError:
                continue
            assert state != "Z", f"backend child {pid} is a zombie"
        if adopted:
            break
        time.sleep(0.02)

    assert adopted, f"no orphan was reparented to backend PID {backend_pid}"
    PY
    """)
    machine.sleep(3)
    machine.succeed(r"""
      backend_pid=$(systemctl show --property MainPID --value container-init-reaper.service)
      python3 - "$backend_pid" <<'PY'
    import pathlib
    import sys

    backend_pid = int(sys.argv[1])
    pid_file = pathlib.Path("/run/container-init-reaper/pids")
    children_file = pathlib.Path(f"/proc/{backend_pid}/task/{backend_pid}/children")

    def state(pid):
        return pathlib.Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]

    for line in pid_file.read_text().splitlines():
        pid = int(line)
        try:
            child_state = state(pid)
        except FileNotFoundError:
            continue
        assert child_state != "Z", f"exited process {pid} remained a zombie"

    for token in children_file.read_text().split():
        pid = int(token)
        try:
            child_state = state(pid)
        except FileNotFoundError:
            continue
        assert child_state != "Z", f"backend child {pid} is a zombie"
    PY
    """)
    machine.succeed("systemctl stop container-init-reaper.service")
  '';
}
