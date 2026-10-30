use {
    super::BackendRuntime,
    crate::{BackendInitial, ConfigApi},
    dev_env_model::{BackendResponse, IdentityRequest, RequestContext, RequestMode},
    dev_env_shell::{build_invocation, CommandLine, ConfiguredShellAdapter, ShellInvocation},
    libc::{geteuid, gid_t, setgid, setgroups, setpgid, setuid},
    std::{
        env,
        io::Error as IoError,
        path::{Path, PathBuf},
        sync::Arc,
    },
};

pub(super) struct InitialRuntime;

impl InitialRuntime {
    pub(super) fn run(runtime: Arc<BackendRuntime>, initial: BackendInitial) {
        let snapshot = runtime.current_snapshot();
        let request_id = "initial-runtime";
        let service_command = env::var_os("DEVENV_BACKEND_SERVICE_COMMAND").map(PathBuf::from);
        let root_service = matches!(
            &initial,
            BackendInitial::Exec(args)
                if args.first().is_some_and(|arg| service_command.as_deref() == Some(Path::new(arg)))
        );
        let initial_user = if root_service {
            "root".to_owned()
        } else {
            env::var("CONTAINER_INIT_HANDOFF_USER").unwrap_or_else(|_| "dev".to_owned())
        };
        let context = RequestContext::new(
            request_id,
            RequestMode::Shell,
            snapshot.workspace_root().clone(),
            snapshot.resolved_config().shell().default().to_owned(),
        )
        .with_identity(IdentityRequest::User { name: initial_user })
        .with_runtime_inputs(
            snapshot
                .runtime_inputs()
                .keys()
                .filter_map(|name| env::var(name).ok().map(|value| (name.clone(), value))),
        )
        .with_ambient_environment(ConfigApi::ambient_environment());
        let response = runtime.prepare_initial(&context);
        let prepared = match response.response() {
            BackendResponse::Prepared(prepared) => prepared.as_ref().clone(),
            BackendResponse::Error(error) => {
                runtime.fail_with_class(
                    error.class(),
                    format!(
                        "initial runtime preparation failed: {}: {}",
                        error.class(),
                        error.message()
                    ),
                );
                return;
            }
            other => {
                runtime.fail(format!(
                    "initial runtime preparation returned an unexpected response: {other:?}"
                ));
                return;
            }
        };
        let command: Result<CommandLine, ()> = match initial {
            BackendInitial::Shell => {
                let shell = snapshot.resolved_config().shell().default().to_owned();
                let Some(config) = snapshot.shells().get(&shell) else {
                    runtime.fail("initial shell is not configured".to_owned());
                    return;
                };
                let adapter = ConfiguredShellAdapter::new(shell);
                build_invocation(&adapter, config, &ShellInvocation::interactive(Vec::new()))
                    .map_err(|_| ())
            }
            BackendInitial::Exec(args) => {
                let mut args = args.into_iter();
                let Some(program) = args.next() else {
                    runtime.fail("initial command is empty".to_owned());
                    return;
                };
                CommandLine::try_new(program, args.collect::<Vec<_>>()).map_err(|_| ())
            }
        };
        let Ok(command) = command else {
            runtime.fail("initial runtime argv is invalid".to_owned());
            return;
        };
        let (program, _args) = command.clone().into_parts();
        let Ok(mut command) = command.command_with_environment(&prepared.materialized_environment)
        else {
            runtime.fail("could not build initial runtime".to_owned());
            return;
        };
        command
            .env("HOME", prepared.identity.home())
            .env("USER", prepared.identity.user())
            .env("LOGNAME", prepared.identity.user());
        command.current_dir(&prepared.cwd);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let identity = prepared.identity.clone();
            let groups = prepared.supplemental_groups.clone();
            unsafe {
                command.pre_exec(move || {
                    if setpgid(0, 0) == -1 {
                        return Err(IoError::last_os_error());
                    }
                    if identity.uid() != 0 && geteuid() == 0 {
                        let groups = groups
                            .iter()
                            .map(|group| *group as gid_t)
                            .collect::<Vec<_>>();
                        if setgroups(groups.len(), groups.as_ptr()) != 0 {
                            return Err(IoError::last_os_error());
                        }
                        if setgid(identity.gid()) != 0 || setuid(identity.uid()) != 0 {
                            return Err(IoError::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                runtime.fail(format!(
                    "could not spawn initial runtime {}: {error}",
                    program.display()
                ));
                return;
            }
        };
        runtime.mark_initial_running(child.id());
        match child.wait() {
            Ok(status) => runtime.mark_initial_exited(status),
            Err(error) => runtime.fail(format!(
                "initial runtime {} failed: {error}",
                program.display()
            )),
        }
        runtime.finish_initial_runtime();
    }
}
