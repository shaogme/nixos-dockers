use crate::args::{Cli, CliCommand, ParseError};
use crate::config;
use crate::doctor;
use crate::error::CliError;
use crate::lock;
use crate::output;
use container_init_core::{ExecutionOptions, IdentityResolver, PlanExecutor, SshCapability};
use std::env;
use std::path::PathBuf;

pub fn run<I>(arguments: I) -> Result<(), CliError>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    let cli = Cli::parse(arguments).map_err(parse_error)?;
    match cli.command {
        CliCommand::Help => {
            println!("{}", crate::args::usage(""));
            Ok(())
        }
        CliCommand::Version => {
            println!("container-init {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        CliCommand::Plan { json } => {
            let loaded = config::load(&cli.options)?;
            output::print_plan(&loaded, json)
        }
        CliCommand::Doctor { json } => {
            let loaded = config::load(&cli.options)?;
            let context = config::runtime_context(&cli.options, false)?;
            let report = doctor::inspect(&loaded, &context);
            output::print_doctor(&report, json)?;
            if !report.ok {
                let check = report
                    .checks
                    .into_iter()
                    .find(|check| check.status == doctor::DoctorStatus::Fail)
                    .expect("a failed doctor report must contain a failed check");
                return Err(CliError::DoctorFailed {
                    check: check.name,
                    source: check.error.map(Box::new),
                });
            }
            Ok(())
        }
        CliCommand::Run { command } => {
            let loaded = config::load(&cli.options)?;
            let context = config::runtime_context(&cli.options, true)?;
            let identity = IdentityResolver::new().resolve(loaded.config(), &context)?;
            let lock_path = lock::path(&cli.options, loaded.profile(), context.cwd(), &identity);
            lock::ensure_parent(&lock_path)?;
            let mut execution_options = ExecutionOptions::default().with_lock_path(lock_path);
            if loaded
                .plan()
                .actions()
                .iter()
                .any(|action| action.kind == bootstrap_model::ActionKind::ServiceSshPrepare)
            {
                let keygen = loaded
                    .config()
                    .actions
                    .iter()
                    .find(|action| action.kind == bootstrap_model::ActionKind::ServiceSshPrepare)
                    .and_then(|action| action.ssh_keygen.clone())
                    .unwrap_or_else(|| SshCapability::default().keygen().display().to_string());
                execution_options = execution_options.with_ssh(SshCapability::new(keygen));
            }
            if let Some(path) = cli
                .options
                .receipt_path
                .clone()
                .or_else(|| env::var_os("CONTAINER_INIT_RECEIPT_PATH").map(PathBuf::from))
            {
                execution_options = execution_options.with_receipt_path(path);
            }
            PlanExecutor::new(loaded.config().clone(), context)
                .with_options(execution_options)
                .execute_and_handoff(loaded.plan(), &command)?;
            Ok(())
        }
        CliCommand::Exec { command } => {
            let loaded = config::load(&cli.options)?;
            let context = config::runtime_context(&cli.options, true)?;
            PlanExecutor::new(loaded.config().clone(), context).exec_and_handoff(&command)?;
            Ok(())
        }
    }
}
fn parse_error(error: ParseError) -> CliError {
    match error {
        ParseError::Invalid(message) => CliError::Arguments(message),
    }
}
