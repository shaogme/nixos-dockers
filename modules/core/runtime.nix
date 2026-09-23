{ config, lib, pkgs, ... }:
let
  containerInitPath = "/usr/bin/container-init";
  bootstrapRealShellPath = "/usr/local/libexec/dev-env/real/bash";

  containerInit = pkgs.rustPlatform.buildRustPackage {
    pname = "container-init";
    version = "0.1.0";
    src = ../../container-init;
    cargoLock.lockFile = ../../container-init/Cargo.lock;
    cargoBuildFlags = [ "--package" "container-init-cli" ];
    doCheck = false;
    installPhase = ''
      install -Dm755 target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/container-init $out/bin/container-init
    '';
  };

  devEnv = pkgs.rustPlatform.buildRustPackage {
    pname = "dev-env";
    version = "0.1.0";
    src = ../../dev-env;
    cargoLock.lockFile = ../../dev-env/Cargo.lock;
    cargoBuildFlags = [ "--package" "dev-env-cli" ];
    doCheck = false;
    installPhase = ''
      install -Dm755 target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/dev-env $out/bin/dev-env
    '';
  };

  runtimeContents = pkgs.runCommand "nixos-docker-runtime-contents" { } ''
    mkdir -p $out/bin $out/usr/bin
    ln -s ${containerInit}/bin/container-init $out/bin/container-init
    ln -s ${devEnv}/bin/dev-env $out/bin/dev-env
    ln -s ${containerInit}/bin/container-init $out/usr/bin/container-init
    ln -s ${devEnv}/bin/dev-env $out/usr/bin/dev-env
    ln -s dev-env $out/bin/dev-env-login-shell
    ln -s dev-env $out/usr/bin/dev-env-login-shell
  '';

  tomlString = value: builtins.toJSON (toString value);
  isEnvironmentVariableName = name:
    builtins.match "[A-Z_][A-Z0-9_]*" name != null;
  environmentVariables = lib.filterAttrs
    (name: value: value != null && isEnvironmentVariableName name)
    config.environment.variables;
  environmentVariableLines = lib.concatStringsSep "\n" (
    lib.mapAttrsToList (name: value: "${name} = ${tomlString value}") environmentVariables
  );

  sshBootstrap = lib.optionalString config.services.openssh.enable ''
    ssh_daemon = "/bin/sshd"
    login_shell = "/usr/bin/dev-env-login-shell"
  '';

  sshActions = lib.optionalString config.services.openssh.enable ''
    [[bootstrap.actions]]
    id = "prepare-ssh"
    kind = "service.ssh.prepare"
    host_key_dir = "/etc/ssh"
    authorized_keys_dir = "/etc/ssh/authorized_keys"
    runtime_dir = "/run/sshd"
    host_key_types = ["rsa", "ed25519"]
    authorized_keys_source = "/tmp/id_ed25519.pub"
    ssh_keygen = "/bin/ssh-keygen"
    run_as = "root"

    [[bootstrap.actions]]
    id = "set-login-shell"
    kind = "process.set_user_shell"
    user = "identity.target"
    shell = "/usr/bin/dev-env-login-shell"
    run_as = "root"
  '';

  profile = pkgs.writeTextDir "etc/dev-env/profiles.d/00-nixos-docker.toml" ''
    schema = 1
    id = "nixos-docker"

    [policy]
    merge = "strict"
    workspace_can_override = ["shell.default"]
    cli_can_override = ["shell.default"]
    unknown_input = "error"
    untrusted_workspace = "prompt"

    [config.workspace]
    root = "/workspace"
    search = "upward"

    [config.shell]
    default = "bash"

    [config.shells.bash]
    command = "/usr/local/libexec/dev-env/real/bash"
    kind = "posix"
    login_args = ["-l"]
    interactive_args = ["-i"]
    command_arg = "-c"

    [config.environment]
    inherit_process = true
    configured_value_precedence = "locked"

    [config.environment.variables]
    ${environmentVariableLines}

    [config.environment.path]
    prepend = ["/nix/var/nix/profiles/default/bin"]
    append = ["/usr/local/bin", "/usr/bin", "/bin"]

    [bootstrap]
    schema = 1
    mode = "strict"
    workspace_root = "/workspace"
    allow_workspace_overlay = false
    non_interactive = "deny"

    [bootstrap.identity]
    default_user = "dev"
    default_uid = 1000
    default_gid = 1000
    default_home = "/home/user"
    auto_mapping = true
    run_as_root_input = "RUN_AS_ROOT"
    uid_input = "HOST_UID"
    gid_input = "HOST_GID"
    home_input = "CONTAINER_HOME"

    [bootstrap.handoff]
    runtime = "/usr/bin/dev-env"
    exec_prefix = ["exec", "--"]
    shell_prefix = ["shell"]
    ${sshBootstrap}

    [bootstrap.policy]
    workspace_safe_action_kinds = ["filesystem.ensure_dir", "filesystem.ensure_symlink"]
    admin_only_action_kinds = ["identity.map_user", "process.set_user_shell", "process.drop_privileges", "service.ssh.prepare", "handoff.exec"]

    [bootstrap.inputs.HOST_UID]
    target = "identity.uid"
    type = "uid_pair"
    namespace = "host"
    runtime = true
    format = "uid[:gid]"

    [bootstrap.inputs.HOST_GID]
    target = "identity.gid"
    type = "gid"
    namespace = "host"
    runtime = true

    [bootstrap.inputs.RUN_AS_ROOT]
    target = "identity.run_as_root"
    type = "bool"
    runtime = true
    default = false

    [bootstrap.inputs.CONTAINER_HOME]
    target = "identity.home"
    type = "path"
    runtime = true
    allow_outside_workspace = true

    [[bootstrap.actions]]
    id = "dev-env-locks"
    kind = "filesystem.ensure_dir"
    path = "/run/dev-env/locks"
    mode = "1777"
    owner = "root"
    run_as = "root"

    [[bootstrap.actions]]
    id = "resolve-identity"
    kind = "identity.resolve"
    run_as = "root"

    [[bootstrap.actions]]
    id = "map-user"
    kind = "identity.map_user"
    when = "identity.uid != '0'"
    run_as = "root"

    [[bootstrap.actions]]
    id = "ensure-home"
    kind = "identity.ensure_home"
    path = "''${identity.home}"
    mode = "0755"
    owner = "identity.target"
    run_as = "root"

    [[bootstrap.actions]]
    id = "drop-privileges"
    kind = "process.drop_privileges"
    when = "identity.uid != '0'"
    run_as = "root"

    [[bootstrap.actions]]
    id = "handoff"
    kind = "handoff.exec"
    run_as = "current"
    ${sshActions}
  '';

  defaultProfile = pkgs.writeTextDir "etc/dev-env/default-profile" "nixos-docker\n";
in
{
  options.runtime = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = config.docker.role != "builder";
      description = "Install the container-init and dev-env runtimes and their base profile.";
    };
  };

  config = lib.mkIf config.runtime.enable {
    environment.variables = {
      DEVENV_CONTAINER_INIT = containerInitPath;
      DEVENV_BOOTSTRAP_REAL_SHELL = bootstrapRealShellPath;
    };
    docker.extraContents = [ runtimeContents profile defaultProfile ];
    docker.extraCommands = ''
      mkdir -p usr/local/libexec/dev-env/real
      ln -sf ${pkgs.bashInteractive}/bin/bash usr/local/libexec/dev-env/real/bash
      ln -sf ${pkgs.bashInteractive}/bin/bash usr/local/libexec/dev-env/real/sh
      # /bin/bash is the compatibility shim. The real shell is kept outside
      # the shim path so invoking it cannot recurse into dev-env.
      rm -f bin/bash
      ln -sf /usr/bin/dev-env bin/bash
      rm -f usr/bin/bash
      ln -sf dev-env usr/bin/bash
      # Keep /bin/sh as a plain POSIX shell. Commands handed off through
      # dev-env are still materialized; --entrypoint /bin/sh remains useful
      # for low-level image diagnostics without invoking the runtime.
      rm -f bin/sh
      ln -sf /usr/local/libexec/dev-env/real/sh bin/sh
    '';
  };
}
