use {
    crate::{
        BackendApi, BootstrapError, BootstrapPathError, Cli, CliCommand, CliError, CliOptions,
        ConfigApi, IoOperation, OfflineApi, OutputApi, OutputError, OutputFormat, ProcessApi,
        TrustApi,
    },
    dev_env_model::{
        BackendRequest, BackendResponse, PreparedResponse, RequestContext, RequestMode, TrustTarget,
    },
    dev_env_shell::{CommandLine, Shim},
    serde_json::{to_string, to_string_pretty, Value as JsonValue},
    std::{
        env::{current_dir, var, var_os},
        ffi::OsString,
        fs,
        io::{Error as IoError, ErrorKind},
        path::{Component, Path, PathBuf},
    },
};

pub struct Application;

impl Application {
    pub fn run<I>(arguments: I) -> Result<i32, CliError>
    where
        I: IntoIterator<Item = OsString>,
    {
        run_inner(arguments)
    }
}

fn run_inner<I>(arguments: I) -> Result<i32, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let cli = Cli::parse(arguments)?;
    let (options, command) = cli.into_parts();
    match command {
        CliCommand::Help => {
            OutputApi::write_stdout(&Cli::usage(""))?;
            OutputApi::write_stdout("\n")?;
            Ok(0)
        }
        CliCommand::Version => {
            OutputApi::print_version()?;
            Ok(0)
        }
        CliCommand::Backend { command } => BackendApi::dispatch(command, options),
        CliCommand::Exec { command } => run_exec(&options, command),
        CliCommand::Shell { shell, login, args } => {
            run_shell(&options, shell.as_deref(), login, args)
        }
        CliCommand::LoginShell { args } => run_shell(&options, None, true, args),
        CliCommand::Shim { shell, real, args } => run_shim(&options, &shell, &real, args),
        CliCommand::Print { format, shell } => run_print(&options, format, shell.as_deref()),
        CliCommand::Explain { path, json } => {
            run_backend_value(&options, BackendRequest::Explain { path }, json)
        }
        CliCommand::Doctor { json } => run_doctor(&options, json),
        CliCommand::Trust { target } => {
            if options.offline {
                return OfflineApi::trust(&target);
            }
            ensure_backend_options(&options)?;
            let hash = TrustApi::digest(&target)?;
            let target = parse_trust_target(&target)?;
            let response = BackendApi::request_backend(BackendRequest::Trust { target })?;
            match response.response() {
                BackendResponse::Trusted { .. } => {
                    OutputApi::write_stdout(&format!("trusted: {hash}\n"))?;
                    Ok(0)
                }
                BackendResponse::Error(error) => Err(CliError::Backend(format!(
                    "{}: {}",
                    error.class(),
                    error.message()
                ))),
                other => Err(CliError::Backend(format!(
                    "backend returned unexpected response: {other:?}"
                ))),
            }
        }
    }
}

fn run_exec(options: &CliOptions, command: Vec<OsString>) -> Result<i32, CliError> {
    if options.offline {
        return OfflineApi::exec(options, command);
    }
    ensure_backend_options(options)?;
    let mut arguments = command.into_iter();
    let program = arguments
        .next()
        .expect("argument parser requires a command");
    let line = CommandLine::try_new(program, arguments.collect::<Vec<_>>())?;
    let prepared = prepare_client(options, RequestMode::Exec, None, Vec::new())?;
    ProcessApi::execute(line, &prepared.materialized_environment, &prepared.cwd)
}

fn run_shell(
    options: &CliOptions,
    shell: Option<&str>,
    login: bool,
    args: Vec<OsString>,
) -> Result<i32, CliError> {
    if options.offline {
        return OfflineApi::shell(options, shell, login, args);
    }
    ensure_backend_options(options)?;
    let mode = if login {
        RequestMode::LoginShell
    } else {
        RequestMode::Shell
    };
    let prepared = prepare_client(options, mode, shell, args.clone())?;
    let line = prepared_command_line(&prepared)?;
    ProcessApi::execute(line, &prepared.materialized_environment, &prepared.cwd)
}

fn run_shim(
    options: &CliOptions,
    shell: &str,
    real: &Path,
    args: Vec<OsString>,
) -> Result<i32, CliError> {
    if options.offline {
        return OfflineApi::shim(options, shell, real, args);
    }
    ensure_backend_options(options)?;
    if ConfigApi::effective_user_id() == 0 {
        if let Some(paths) = BootstrapPaths::from_environment()? {
            let line = paths.build_command(&args)?;
            return ProcessApi::execute_inherited(line);
        }
    }

    let prepared = prepare_client(options, RequestMode::Shim, Some(shell), args.clone())?;
    // The configured shell command is the real executable that the shim must
    // launch, so it is expected to equal `real` for the image-level bash
    // compatibility shim.  Passing it as the recursion guard would reject
    // every valid invocation.  The shim path itself is owned by the image
    // layout and is not the configured real shell command.
    let line = Shim::new(real).build(&args)?;
    ProcessApi::execute(line, &prepared.materialized_environment, &prepared.cwd)
}

fn ensure_backend_options(options: &CliOptions) -> Result<(), CliError> {
    if options.profile.is_some()
        || options.profiles_dir.is_some()
        || options.admin_profiles_dir.is_some()
        || options.default_profile_file.is_some()
        || options.workspace.is_some()
        || options.config.is_some()
        || !options.patches.is_empty()
    {
        return Err(CliError::Backend(
            "profile and configuration options are only valid for backend run/reload or --offline"
                .to_owned(),
        ));
    }
    Ok(())
}

fn prepare_client(
    options: &CliOptions,
    mode: RequestMode,
    shell: Option<&str>,
    shell_args: Vec<OsString>,
) -> Result<PreparedResponse, CliError> {
    let cwd = options
        .cwd
        .clone()
        .or_else(|| var_os("DEVENV_CWD").map(PathBuf::from))
        .unwrap_or(
            current_dir()
                .map_err(|source| CliError::io(IoOperation::ReadCurrentDirectory, None, source))?,
        );
    let cwd = if cwd.is_absolute() {
        cwd
    } else {
        current_dir()
            .map_err(|source| CliError::io(IoOperation::ReadCurrentDirectory, None, source))?
            .join(cwd)
    };
    let shell = shell
        .map(str::to_owned)
        .or_else(|| var("DEVENV_SHELL").ok())
        .unwrap_or_else(|| "default".to_owned());
    let shell_args = shell_args
        .into_iter()
        .map(|arg| {
            arg.into_string().map_err(|_| {
                CliError::Backend(
                    "shell arguments must be valid UTF-8 for backend protocol".to_owned(),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let context = RequestContext::new("client", mode, normalize_path(&cwd), shell)
        .with_shell_args(shell_args)
        .with_ambient_environment(ConfigApi::ambient_environment());
    BackendApi::prepare(context)
}

fn prepared_command_line(prepared: &PreparedResponse) -> Result<CommandLine, CliError> {
    CommandLine::try_new(
        prepared.shell_invocation.executable.clone(),
        prepared
            .shell_invocation
            .args
            .iter()
            .cloned()
            .map(OsString::from)
            .collect::<Vec<_>>(),
    )
    .map_err(CliError::CommandLine)
}

fn run_print(
    options: &CliOptions,
    format: OutputFormat,
    shell: Option<&str>,
) -> Result<i32, CliError> {
    if options.offline {
        return OfflineApi::print(options, format, shell);
    }
    ensure_backend_options(options)?;
    let prepared = prepare_client(options, RequestMode::Print, shell, Vec::new())?;
    OutputApi::print_materialized_environment(
        &prepared.materialized_environment,
        format,
        options.show_secrets,
    )?;
    Ok(0)
}

fn run_backend_value(
    options: &CliOptions,
    request: BackendRequest,
    json: bool,
) -> Result<i32, CliError> {
    if options.offline {
        if let BackendRequest::Explain { ref path } = request {
            return OfflineApi::explain(options, path.as_deref(), json);
        }
    }
    ensure_backend_options(options)?;
    let response = BackendApi::request_backend(request)?;
    let value = match response.response() {
        BackendResponse::Explain(value) | BackendResponse::Plan(value) => value,
        BackendResponse::Error(error) => {
            return Err(CliError::Backend(format!(
                "{}: {}",
                error.class(),
                error.message()
            )))
        }
        other => {
            return Err(CliError::Backend(format!(
                "backend returned unexpected response: {other:?}"
            )))
        }
    };
    let rendered = if json {
        to_string_pretty(&value)
    } else {
        to_string(&value)
    }
    .map_err(OutputError::Json)?;
    OutputApi::write_stdout(&format!("{rendered}\n"))?;
    Ok(0)
}

fn run_doctor(options: &CliOptions, json: bool) -> Result<i32, CliError> {
    if options.offline {
        return OfflineApi::doctor(options, json);
    }
    ensure_backend_options(options)?;
    let response = BackendApi::request_backend(BackendRequest::Doctor)?;
    let value = match response.response() {
        BackendResponse::Doctor(value) => value,
        BackendResponse::Error(error) => {
            return Err(CliError::Backend(format!(
                "{}: {}",
                error.class(),
                error.message()
            )))
        }
        other => {
            return Err(CliError::Backend(format!(
                "backend returned unexpected response: {other:?}"
            )))
        }
    };
    if json {
        OutputApi::write_stdout(&format!(
            "{}\n",
            to_string_pretty(&value).map_err(OutputError::Json)?
        ))?;
    } else {
        OutputApi::write_stdout(&format!(
            "status: {}\n",
            if value
                .get("ok")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false)
            {
                "ok"
            } else {
                "failed"
            }
        ))?;
    }
    if value.get("ok").and_then(JsonValue::as_bool) == Some(true) {
        Ok(0)
    } else {
        Err(CliError::DoctorFailed { failed_checks: 1 })
    }
}

fn parse_trust_target(target: &Path) -> Result<TrustTarget, CliError> {
    let value = target.to_string_lossy();
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        let mut digest = [0_u8; 32];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            digest[index] = (hex_value(chunk[0]) << 4) | hex_value(chunk[1]);
        }
        Ok(TrustTarget::Sha256 { digest })
    } else {
        let path = if target.is_absolute() {
            target.to_path_buf()
        } else {
            current_dir()
                .map_err(|source| CliError::io(IoOperation::ReadCurrentDirectory, None, source))?
                .join(target)
        };
        Ok(TrustTarget::Path {
            path: normalize_path(&path),
        })
    }
}

fn hex_value(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        b'A'..=b'F' => value - b'A' + 10,
        _ => 0,
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => output.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            Component::Normal(value) => output.push(value),
        }
    }
    output
}

const CONTAINER_INIT_ENV: &str = "DEVENV_CONTAINER_INIT";
const BOOTSTRAP_REAL_SHELL_ENV: &str = "DEVENV_BOOTSTRAP_REAL_SHELL";

struct BootstrapPaths {
    container_init: PathBuf,
    real_shell: PathBuf,
}

impl BootstrapPaths {
    fn from_environment() -> Result<Option<Self>, CliError> {
        let container_init = var_os(CONTAINER_INIT_ENV);
        let real_shell = var_os(BOOTSTRAP_REAL_SHELL_ENV);
        if container_init.is_none() && real_shell.is_none() {
            return Ok(None);
        }

        let container_init = container_init
            .ok_or(BootstrapError::MissingVariable {
                variable: CONTAINER_INIT_ENV,
            })
            .map(PathBuf::from)?;
        let real_shell = real_shell
            .ok_or(BootstrapError::MissingVariable {
                variable: BOOTSTRAP_REAL_SHELL_ENV,
            })
            .map(PathBuf::from)?;

        validate_bootstrap_executable(CONTAINER_INIT_ENV, &container_init)?;
        validate_bootstrap_executable(BOOTSTRAP_REAL_SHELL_ENV, &real_shell)?;
        if real_shell == Path::new("/bin/bash") || real_shell == Path::new("/usr/bin/bash") {
            return Err(BootstrapError::RecursiveShell { path: real_shell }.into());
        }

        Ok(Some(Self {
            container_init,
            real_shell,
        }))
    }

    fn build_command(&self, args: &[OsString]) -> Result<CommandLine, CliError> {
        let mut bootstrap_args = Vec::with_capacity(args.len() + 3);
        bootstrap_args.push(OsString::from("exec"));
        bootstrap_args.push(OsString::from("--"));
        bootstrap_args.push(self.real_shell.clone().into_os_string());
        bootstrap_args.extend(args.iter().cloned());
        CommandLine::try_new(self.container_init.clone(), bootstrap_args)
            .map_err(CliError::CommandLine)
    }
}

fn validate_bootstrap_executable(variable: &'static str, path: &Path) -> Result<(), CliError> {
    let reason = if path.as_os_str().is_empty() {
        Some(BootstrapPathError::Empty)
    } else if contains_nul(path) {
        Some(BootstrapPathError::Nul)
    } else if !path.is_absolute() {
        Some(BootstrapPathError::Relative)
    } else {
        None
    };
    if let Some(reason) = reason {
        return Err(BootstrapError::InvalidPath {
            variable,
            path: path.to_path_buf(),
            reason,
        }
        .into());
    }

    let metadata = fs::metadata(path).map_err(|source| BootstrapError::NotExecutable {
        variable,
        path: path.to_path_buf(),
        source,
    })?;
    let executable = metadata.is_file() && {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    };
    if !executable {
        return Err(BootstrapError::NotExecutable {
            variable,
            path: path.to_path_buf(),
            source: IoError::new(
                ErrorKind::PermissionDenied,
                "path is not a regular executable file",
            ),
        }
        .into());
    }
    Ok(())
}

fn contains_nul(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().contains(&0)
    }
    #[cfg(not(unix))]
    {
        path.as_os_str().to_string_lossy().contains('\0')
    }
}
