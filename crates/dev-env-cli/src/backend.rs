mod server;

use {
    crate::{BackendCommand, BackendSupport, CliError, CliOptions},
    dev_env_model::{
        BackendRequest, BackendResponse, BackendResponseMessage, BackendStatus, PreparedResponse,
        RequestContext,
    },
    server::BackendServer,
};

pub struct BackendApi;

impl BackendApi {
    pub fn dispatch(command: BackendCommand, options: CliOptions) -> Result<i32, CliError> {
        match command {
            BackendCommand::Run { initial } => BackendServer::run(initial, options),
            BackendCommand::Status { json: _ } => {
                let response = BackendSupport::request(BackendRequest::Status)?;
                BackendSupport::print_response(response)?;
                Ok(0)
            }
            BackendCommand::WaitReady => {
                let status = Self::wait_ready(options.startup_timeout_ms)?;
                BackendSupport::print_status(status)?;
                Ok(0)
            }
            BackendCommand::Reload { wait } => {
                let response = BackendSupport::request(BackendRequest::Reload { wait })?;
                BackendSupport::print_response(response)?;
                Ok(0)
            }
            BackendCommand::Stop => {
                let response = BackendSupport::request(BackendRequest::Stop)?;
                BackendSupport::print_response(response)?;
                Ok(0)
            }
        }
    }

    pub fn prepare(context: RequestContext) -> Result<PreparedResponse, CliError> {
        Self::prepare_with_startup_timeout(context, None)
    }

    pub fn prepare_with_startup_timeout(
        context: RequestContext,
        timeout_ms: Option<u64>,
    ) -> Result<PreparedResponse, CliError> {
        let response = BackendSupport::request_with_startup_timeout(
            BackendRequest::Prepare { context },
            timeout_ms,
        )?;
        match response.response() {
            BackendResponse::Prepared(prepared) => {
                if !prepared.cwd.is_absolute()
                    || !prepared.shell_invocation.executable.is_absolute()
                    || prepared
                        .shell_invocation
                        .args
                        .iter()
                        .any(|arg| arg.contains('\0'))
                {
                    return Err(CliError::Backend(
                        "backend returned an invalid prepared invocation".to_owned(),
                    ));
                }
                prepared
                    .materialized_environment
                    .validate()
                    .map_err(|error| CliError::Backend(error.to_string()))?;
                Ok((**prepared).clone())
            }
            BackendResponse::Error(error) if error.class() == "DEVENV-E-PROVIDER" => {
                Err(CliError::BackendProvider(error.message().to_owned()))
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

    pub fn wait_ready(timeout_ms: Option<u64>) -> Result<BackendStatus, CliError> {
        BackendSupport::await_backend_ready(timeout_ms)
    }

    pub fn request_backend(message: BackendRequest) -> Result<BackendResponseMessage, CliError> {
        BackendSupport::request_backend_inner(message)
    }

    pub fn request_backend_with_startup_timeout(
        message: BackendRequest,
        timeout_ms: Option<u64>,
    ) -> Result<BackendResponseMessage, CliError> {
        BackendSupport::request_with_startup_timeout(message, timeout_ms)
    }
}
