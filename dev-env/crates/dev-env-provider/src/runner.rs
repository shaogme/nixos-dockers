use crate::command::{
    CommandError, CommandExecutor, CommandOutput, CommandRequest, ProcessExecutor,
};
use crate::condition::evaluate;
use crate::context::ProviderContext;
use crate::detect::{detect, DetectionResult, ExecutableLocator, SystemExecutableLocator};
use crate::environment::{parse_output, EnvironmentDelta};
use crate::error::ProviderRuntimeError;
use crate::lock::{lock_key, LockManager, ProviderLockGuard};
use crate::receipt::{provider_config_fingerprint, workspace_fingerprint};
use crate::template::expand_argv;
use dev_env_model::{
    Condition, FailurePolicy, MaterializedEnv, MissingProviderPolicy, PrepareStep, ProviderConfig,
    ProviderReceipt, Sensitivity,
};
use serde::Serialize;
use std::time::Duration;

/// A warning is intentionally an event-shaped value.  It keeps command
/// output as bytes and status as fields, rather than storing a formatted
/// representation of a structured error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ProviderDiagnostic {
    MissingExecutable {
        provider: String,
        executable: String,
        policy: MissingProviderPolicy,
    },
    NotApplicable {
        provider: String,
    },
    PrepareFailed {
        provider: String,
        step: usize,
        policy: FailurePolicy,
        status: Option<i32>,
        timed_out: bool,
        stderr: Vec<u8>,
    },
    ShellenvFailed {
        provider: String,
        policy: FailurePolicy,
        status: Option<i32>,
        timed_out: bool,
        stderr: Vec<u8>,
    },
    ShellenvRejected {
        provider: String,
        policy: FailurePolicy,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderRunResult {
    pub provider: String,
    pub sensitivity: Sensitivity,
    pub detection: DetectionResult,
    pub prepared_steps: usize,
    pub environment: EnvironmentDelta,
    pub diagnostics: Vec<ProviderDiagnostic>,
    pub receipt: Option<ProviderReceipt>,
}

impl ProviderRunResult {
    pub fn apply_to(&self, environment: &mut MaterializedEnv) {
        self.environment
            .apply_to_materialized(environment, &self.provider, self.sensitivity);
        if let Some(receipt) = &self.receipt {
            environment.provider_receipts.push(receipt.clone());
        }
    }
}

pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn run(&self, context: &ProviderContext) -> Result<ProviderRunResult, ProviderRuntimeError>;
}

pub struct GenericProvider {
    id: String,
    config: ProviderConfig,
    runner: ProviderRunner,
}

impl GenericProvider {
    pub fn new(id: impl Into<String>, config: ProviderConfig, runner: ProviderRunner) -> Self {
        Self {
            id: id.into(),
            config,
            runner,
        }
    }

    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }
}

impl Provider for GenericProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn run(&self, context: &ProviderContext) -> Result<ProviderRunResult, ProviderRuntimeError> {
        self.runner.run(&self.id, &self.config, context)
    }
}

pub struct ProviderRunner {
    executor: Box<dyn CommandExecutor>,
    locator: Box<dyn ExecutableLocator>,
    lock_manager: Option<LockManager>,
    command_timeout: Duration,
    lock_timeout: Duration,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
}

impl Default for ProviderRunner {
    fn default() -> Self {
        Self {
            executor: Box::new(ProcessExecutor),
            locator: Box::new(SystemExecutableLocator),
            lock_manager: Some(LockManager::from_environment()),
            command_timeout: Duration::from_secs(300),
            lock_timeout: Duration::from_secs(60),
            max_stdout_bytes: 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
        }
    }
}

impl ProviderRunner {
    pub fn with_executor<E>(executor: E) -> Self
    where
        E: CommandExecutor + 'static,
    {
        Self {
            executor: Box::new(executor),
            ..Self::default()
        }
    }

    pub fn with_executor_and_locator<E, L>(executor: E, locator: L) -> Self
    where
        E: CommandExecutor + 'static,
        L: ExecutableLocator + 'static,
    {
        Self {
            executor: Box::new(executor),
            locator: Box::new(locator),
            ..Self::default()
        }
    }

    pub fn without_locks(mut self) -> Self {
        self.lock_manager = None;
        self
    }

    pub fn with_lock_manager(mut self, lock_manager: LockManager) -> Self {
        self.lock_manager = Some(lock_manager);
        self
    }

    pub fn with_command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = timeout.max(Duration::from_millis(1));
        self
    }

    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    pub fn with_output_limits(mut self, stdout: usize, stderr: usize) -> Self {
        self.max_stdout_bytes = stdout;
        self.max_stderr_bytes = stderr;
        self
    }

    pub fn run(
        &self,
        provider_id: &str,
        config: &ProviderConfig,
        context: &ProviderContext,
    ) -> Result<ProviderRunResult, ProviderRuntimeError> {
        config
            .validate(provider_id)
            .map_err(ProviderRuntimeError::Model)?;
        let detection = detect(
            config,
            &context.workspace,
            &context.environment,
            self.locator.as_ref(),
        )
        .map_err(|source| ProviderRuntimeError::Detection {
            provider: provider_id.to_owned(),
            source,
        })?;

        if !detection.applicable {
            let mut diagnostics = Vec::new();
            if !config.detect_files.is_empty() && detection.matched_files.is_empty() {
                diagnostics.push(ProviderDiagnostic::NotApplicable {
                    provider: provider_id.to_owned(),
                });
            } else {
                match config.missing {
                    MissingProviderPolicy::Error => {
                        return Err(ProviderRuntimeError::MissingExecutable {
                            provider: provider_id.to_owned(),
                            executable: config.executable.clone(),
                            policy: config.missing,
                        });
                    }
                    MissingProviderPolicy::Warn | MissingProviderPolicy::Ignore => {
                        diagnostics.push(ProviderDiagnostic::MissingExecutable {
                            provider: provider_id.to_owned(),
                            executable: config.executable.clone(),
                            policy: config.missing,
                        });
                    }
                }
            }
            return Ok(ProviderRunResult {
                provider: provider_id.to_owned(),
                sensitivity: config.sensitivity,
                detection,
                prepared_steps: 0,
                environment: EnvironmentDelta::default(),
                diagnostics,
                receipt: None,
            });
        }

        let executable = detection
            .executable
            .as_ref()
            .expect("applicable implies executable");
        let mut provider_context = context.clone();
        provider_context.set_workspace_config_present(
            context.workspace_config_present || !detection.matched_files.is_empty(),
        );
        let mut runnable_steps = Vec::new();
        for (index, step) in config.prepare.iter().enumerate() {
            if condition_matches(step, &provider_context)? {
                runnable_steps.push((index, step));
            }
        }

        let _lock = if runnable_steps.is_empty() || self.lock_manager.is_none() {
            None
        } else {
            Some(self.acquire_lock(provider_id, &context.workspace, context.user_id)?)
        };

        let mut diagnostics = Vec::new();
        let mut prepared_steps = 0;
        for (index, step) in runnable_steps {
            let operation = format!("prepare[{index}]");
            let args = self.expand_args(provider_id, &operation, &step.argv, &provider_context)?;
            let result = self.execute(
                provider_id,
                &operation,
                executable,
                args,
                &provider_context,
                step.timeout_ms,
            );
            match result {
                Ok(output) if output.succeeded() => prepared_steps += 1,
                Ok(output) => {
                    let error = ProviderRuntimeError::CommandFailed {
                        provider: provider_id.to_owned(),
                        operation,
                        output,
                    };
                    if step.failure == FailurePolicy::Error {
                        return Err(error);
                    }
                    diagnostics.push(prepare_diagnostic(provider_id, index, step.failure, &error));
                }
                Err(error) => {
                    if step.failure == FailurePolicy::Error {
                        return Err(error);
                    }
                    diagnostics.push(prepare_diagnostic(provider_id, index, step.failure, &error));
                }
            }
        }

        let mut environment = EnvironmentDelta::default();
        if let Some(shellenv) = &config.shellenv {
            let operation = "shellenv".to_owned();
            let args =
                self.expand_args(provider_id, &operation, &shellenv.argv, &provider_context)?;
            let result = self.execute(
                provider_id,
                &operation,
                executable,
                args,
                &provider_context,
                shellenv.timeout_ms,
            );
            match result {
                Ok(output) if output.succeeded() => {
                    let text = String::from_utf8(output.stdout).map_err(|source| {
                        ProviderRuntimeError::OutputUtf8 {
                            provider: provider_id.to_owned(),
                            operation: "shellenv".to_owned(),
                            source,
                        }
                    });
                    match text.and_then(|text| {
                        parse_output(shellenv.format, &text).map_err(|source| {
                            ProviderRuntimeError::OutputRejected {
                                provider: provider_id.to_owned(),
                                operation: "shellenv".to_owned(),
                                source,
                            }
                        })
                    }) {
                        Ok(entries) => environment.apply_entries(entries),
                        Err(error) => {
                            if shellenv.failure == FailurePolicy::Error {
                                return Err(error);
                            }
                            diagnostics.push(ProviderDiagnostic::ShellenvRejected {
                                provider: provider_id.to_owned(),
                                policy: shellenv.failure,
                            });
                        }
                    }
                }
                Ok(output) => {
                    let error = ProviderRuntimeError::CommandFailed {
                        provider: provider_id.to_owned(),
                        operation: "shellenv".to_owned(),
                        output,
                    };
                    if shellenv.failure == FailurePolicy::Error {
                        return Err(error);
                    }
                    diagnostics.push(shellenv_diagnostic(provider_id, shellenv.failure, &error));
                }
                Err(error) => {
                    if shellenv.failure == FailurePolicy::Error {
                        return Err(error);
                    }
                    diagnostics.push(shellenv_diagnostic(provider_id, shellenv.failure, &error));
                }
            }
        }

        let receipt = if prepared_steps == 0 {
            None
        } else {
            Some(ProviderReceipt {
                provider: provider_id.to_owned(),
                config_fingerprint: if context.config_fingerprint == [0; 32] {
                    provider_config_fingerprint(config).map_err(|source| {
                        ProviderRuntimeError::Fingerprint {
                            provider: provider_id.to_owned(),
                            source,
                        }
                    })?
                } else {
                    context.config_fingerprint
                },
                workspace_fingerprint: workspace_fingerprint(
                    &context.workspace,
                    &detection.matched_files,
                )
                .map_err(|source| ProviderRuntimeError::Fingerprint {
                    provider: provider_id.to_owned(),
                    source,
                })?,
                version: None,
                completed_at: None,
            })
        };

        Ok(ProviderRunResult {
            provider: provider_id.to_owned(),
            sensitivity: config.sensitivity,
            detection,
            prepared_steps,
            environment,
            diagnostics,
            receipt,
        })
    }

    fn acquire_lock(
        &self,
        provider: &str,
        workspace: &std::path::Path,
        user_id: u32,
    ) -> Result<ProviderLockGuard, ProviderRuntimeError> {
        let manager = self
            .lock_manager
            .as_ref()
            .expect("lock manager is configured");
        manager
            .acquire(lock_key(workspace, user_id, provider), self.lock_timeout)
            .map_err(|source| ProviderRuntimeError::Lock {
                provider: provider.to_owned(),
                source,
            })
    }

    fn expand_args(
        &self,
        provider: &str,
        operation: &str,
        args: &[String],
        context: &ProviderContext,
    ) -> Result<Vec<String>, ProviderRuntimeError> {
        expand_argv(args, provider, context).map_err(|source| ProviderRuntimeError::Template {
            provider: provider.to_owned(),
            operation: operation.to_owned(),
            source,
        })
    }

    fn execute(
        &self,
        provider: &str,
        operation: &str,
        executable: &std::path::Path,
        args: Vec<String>,
        context: &ProviderContext,
        timeout_ms: Option<u64>,
    ) -> Result<CommandOutput, ProviderRuntimeError> {
        let mut request = CommandRequest::new(executable.display().to_string(), &context.cwd);
        request.args = args;
        request.environment = context.environment.clone();
        request.timeout = Some(
            timeout_ms
                .map(Duration::from_millis)
                .unwrap_or(self.command_timeout),
        );
        request.max_stdout_bytes = self.max_stdout_bytes;
        request.max_stderr_bytes = self.max_stderr_bytes;
        self.executor
            .execute(&request)
            .map_err(|source| ProviderRuntimeError::Command {
                provider: provider.to_owned(),
                operation: operation.to_owned(),
                source,
            })
    }
}

fn condition_matches(
    step: &PrepareStep,
    context: &ProviderContext,
) -> Result<bool, ProviderRuntimeError> {
    let Some(condition) = step.when.as_deref() else {
        return Ok(true);
    };
    match Condition::parse(condition) {
        Ok(condition) => Ok(evaluate(&condition, context)),
        Err(source) => Err(ProviderRuntimeError::Model(
            dev_env_model::ModelError::InvalidCondition {
                location: "provider prepare condition".to_owned(),
                reason: source,
            },
        )),
    }
}

fn prepare_diagnostic(
    provider: &str,
    step: usize,
    policy: FailurePolicy,
    error: &ProviderRuntimeError,
) -> ProviderDiagnostic {
    let (status, timed_out, stderr) = command_failure_parts(error);
    ProviderDiagnostic::PrepareFailed {
        provider: provider.to_owned(),
        step,
        policy,
        status,
        timed_out,
        stderr,
    }
}

fn shellenv_diagnostic(
    provider: &str,
    policy: FailurePolicy,
    error: &ProviderRuntimeError,
) -> ProviderDiagnostic {
    let (status, timed_out, stderr) = command_failure_parts(error);
    ProviderDiagnostic::ShellenvFailed {
        provider: provider.to_owned(),
        policy,
        status,
        timed_out,
        stderr,
    }
}

fn command_failure_parts(error: &ProviderRuntimeError) -> (Option<i32>, bool, Vec<u8>) {
    match error {
        ProviderRuntimeError::CommandFailed { output, .. } => {
            (output.status, output.timed_out, output.stderr.clone())
        }
        ProviderRuntimeError::Command { source, .. } => match source {
            CommandError::Spawn { .. }
            | CommandError::Wait { .. }
            | CommandError::Read { .. }
            | CommandError::ReaderPanicked { .. }
            | CommandError::Kill { .. }
            | CommandError::InvalidProgram
            | CommandError::InvalidArgument
            | CommandError::InvalidEnvironment
            | CommandError::InvalidTimeout => (None, false, Vec::new()),
        },
        _ => (None, false, Vec::new()),
    }
}
