let
  system = builtins.currentSystem;
  sources = import ../images/rust/npins;
  pkgs = import sources.nixpkgs {
    inherit system;
    config.allowUnfree = true;
  };
  builder = import ../modules { inherit pkgs sources system; };
  defaultRuntime = builder.evalContainer {
    modules = [ { runtime.enable = true; } ];
  };
  overriddenRuntime = builder.evalContainer {
    modules = [
      {
        runtime.enable = true;
        runtime.backendStartupTimeoutMs = 120000;
      }
    ];
  };
in
assert defaultRuntime.config.runtime.backendStartupTimeoutMs == 60000;
assert defaultRuntime.config.environment.variables.DEVENV_STARTUP_TIMEOUT_MS == "60000";
assert overriddenRuntime.config.runtime.backendStartupTimeoutMs == 120000;
assert overriddenRuntime.config.environment.variables.DEVENV_STARTUP_TIMEOUT_MS == "120000";
{
  defaultTimeoutMs = defaultRuntime.config.runtime.backendStartupTimeoutMs;
  injectedDefault = defaultRuntime.config.environment.variables.DEVENV_STARTUP_TIMEOUT_MS;
  overriddenTimeoutMs = overriddenRuntime.config.runtime.backendStartupTimeoutMs;
  injectedOverride = overriddenRuntime.config.environment.variables.DEVENV_STARTUP_TIMEOUT_MS;
}
