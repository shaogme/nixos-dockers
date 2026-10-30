use crate::{
    command::{
        CommandError, CommandExecutor, CommandOutput, CommandRequest, ProcessExecutor,
        ProviderProcessSupervisor,
    },
    condition::ConditionEvaluator,
    context::ProviderContext,
    detect::{DetectionResult, ExecutableLocator, ProviderDetection, SystemExecutableLocator},
    environment::EnvironmentDelta,
    error::ProviderRuntimeError,
    receipt::ProviderFingerprints,
    supervisor::{ProviderJob, ProviderSupervisor},
    template::ProviderTemplate,
};
use dev_env_model::{
    Condition, EffectiveIdentity, FailurePolicy, MaterializedEnv, MissingProviderPolicy,
    ModelError, PrepareStep, ProviderConfig, ProviderReceipt, Sensitivity,
};
use serde::Serialize;
use std::{path::Path, sync::OnceLock, time::Duration};

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
    provider: String,
    sensitivity: Sensitivity,
    detection: DetectionResult,
    prepared_steps: usize,
    environment: EnvironmentDelta,
    diagnostics: Vec<ProviderDiagnostic>,
    receipt: Option<ProviderReceipt>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderRunResultParts {
    pub provider: String,
    pub sensitivity: Sensitivity,
    pub detection: DetectionResult,
    pub prepared_steps: usize,
    pub environment: EnvironmentDelta,
    pub diagnostics: Vec<ProviderDiagnostic>,
    pub receipt: Option<ProviderReceipt>,
}

pub struct ProviderRunner {
    executor: Box<dyn CommandExecutor>,
    locator: Box<dyn ExecutableLocator>,
    command_timeout: Duration,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
}

struct CommandExecution<'a> {
    provider: &'a str,
    operation: &'a str,
    executable: &'a Path,
    args: Vec<String>,
    context: &'a ProviderContext,
    timeout_ms: Option<u64>,
    identity: Option<&'a EffectiveIdentity>,
}

struct CommandFailureInfo {
    status: Option<i32>,
    timed_out: bool,
    stderr: Vec<u8>,
}

impl ProviderRunResult {
    pub fn new(parts: ProviderRunResultParts) -> Self {
        Self {
            provider: parts.provider,
            sensitivity: parts.sensitivity,
            detection: parts.detection,
            prepared_steps: parts.prepared_steps,
            environment: parts.environment,
            diagnostics: parts.diagnostics,
            receipt: parts.receipt,
        }
    }

    pub fn with_sensitivity(mut self, sensitivity: Sensitivity) -> Self {
        self.sensitivity = sensitivity;
        self
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn sensitivity(&self) -> Sensitivity {
        self.sensitivity
    }

    pub fn detection(&self) -> &DetectionResult {
        &self.detection
    }

    pub fn prepared_steps(&self) -> usize {
        self.prepared_steps
    }

    pub fn environment(&self) -> &EnvironmentDelta {
        &self.environment
    }

    pub fn diagnostics(&self) -> &[ProviderDiagnostic] {
        &self.diagnostics
    }

    pub fn receipt(&self) -> Option<&ProviderReceipt> {
        self.receipt.as_ref()
    }

    pub fn apply_to(&self, environment: &mut MaterializedEnv) {
        self.environment
            .apply_to_materialized(environment, &self.provider, self.sensitivity);
        if let Some(receipt) = &self.receipt {
            environment.push_provider_receipt(receipt.clone());
        }
    }
}

pub trait Provider: Send + Sync {
    fn id(&self) -> &str;

    fn run(&self, context: &ProviderContext) -> Result<ProviderRunResult, ProviderRuntimeError>;
}

impl Default for ProviderRunner {
    fn default() -> Self {
        static SUPERVISOR: OnceLock<ProviderProcessSupervisor> = OnceLock::new();
        Self::from_parts(
            Box::new(
                SUPERVISOR
                    .get_or_init(ProviderProcessSupervisor::new)
                    .executor(),
            ),
            Box::new(SystemExecutableLocator),
        )
    }
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

impl ProviderRunner {
    fn from_parts(executor: Box<dyn CommandExecutor>, locator: Box<dyn ExecutableLocator>) -> Self {
        Self {
            executor,
            locator,
            command_timeout: Duration::from_secs(300),
            max_stdout_bytes: 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
        }
    }

    pub fn with_process_executor(executor: ProcessExecutor) -> Self {
        Self::from_parts(Box::new(executor), Box::new(SystemExecutableLocator))
    }

    pub fn with_executor<E>(executor: E) -> Self
    where
        E: CommandExecutor + 'static,
    {
        Self::from_parts(Box::new(executor), Box::new(SystemExecutableLocator))
    }

    pub fn with_executor_and_locator<E, L>(executor: E, locator: L) -> Self
    where
        E: CommandExecutor + 'static,
        L: ExecutableLocator + 'static,
    {
        Self::from_parts(Box::new(executor), Box::new(locator))
    }

    pub fn with_command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = timeout.max(Duration::from_millis(1));
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
        self.run_with_identity(provider_id, config, context, None)
    }

    pub fn run_with_identity(
        &self,
        provider_id: &str,
        config: &ProviderConfig,
        context: &ProviderContext,
        identity: Option<&EffectiveIdentity>,
    ) -> Result<ProviderRunResult, ProviderRuntimeError> {
        config
            .validate(provider_id)
            .map_err(ProviderRuntimeError::Model)?;
        let detection = ProviderDetection::detect(
            config,
            context.workspace(),
            context.environment(),
            self.locator.as_ref(),
        )
        .map_err(|source| ProviderRuntimeError::Detection {
            provider: provider_id.to_owned(),
            source,
        })?;

        if !detection.applicable {
            let mut diagnostics = Vec::new();
            if !config.detect_files().is_empty() && detection.matched_files.is_empty() {
                diagnostics.push(ProviderDiagnostic::NotApplicable {
                    provider: provider_id.to_owned(),
                });
            } else {
                match config.missing() {
                    MissingProviderPolicy::Error => {
                        return Err(ProviderRuntimeError::MissingExecutable {
                            provider: provider_id.to_owned(),
                            executable: config.executable().to_owned(),
                            policy: config.missing(),
                        });
                    }
                    MissingProviderPolicy::Warn | MissingProviderPolicy::Ignore => {
                        diagnostics.push(ProviderDiagnostic::MissingExecutable {
                            provider: provider_id.to_owned(),
                            executable: config.executable().to_owned(),
                            policy: config.missing(),
                        });
                    }
                }
            }
            return Ok(ProviderRunResult::new(ProviderRunResultParts {
                provider: provider_id.to_owned(),
                sensitivity: config.sensitivity(),
                detection,
                prepared_steps: 0,
                environment: EnvironmentDelta::default(),
                diagnostics,
                receipt: None,
            }));
        }

        let executable = detection
            .executable
            .as_ref()
            .expect("applicable implies executable");
        let mut provider_context = context.clone();
        provider_context.set_workspace_config_present(
            context.workspace_config_present() || !detection.matched_files.is_empty(),
        );
        let mut runnable_steps = Vec::new();
        for (index, step) in config.prepare().iter().enumerate() {
            if condition_matches(step, &provider_context)? {
                runnable_steps.push((index, step));
            }
        }

        let mut diagnostics = Vec::new();
        let mut prepared_steps = 0;
        for (index, step) in runnable_steps {
            let operation = format!("prepare[{index}]");
            let args = self.expand_args(provider_id, &operation, step.argv(), &provider_context)?;
            let result = self.execute(CommandExecution {
                provider: provider_id,
                operation: &operation,
                executable,
                args,
                context: &provider_context,
                timeout_ms: step.timeout_ms(),
                identity,
            });
            match result {
                Ok(output) if output.succeeded() => prepared_steps += 1,
                Ok(output) => {
                    let error = ProviderRuntimeError::CommandFailed {
                        provider: provider_id.to_owned(),
                        operation,
                        output,
                    };
                    if step.failure() == FailurePolicy::Error {
                        return Err(error);
                    }
                    diagnostics.push(prepare_diagnostic(
                        provider_id,
                        index,
                        step.failure(),
                        &error,
                    ));
                }
                Err(error) => {
                    if step.failure() == FailurePolicy::Error {
                        return Err(error);
                    }
                    diagnostics.push(prepare_diagnostic(
                        provider_id,
                        index,
                        step.failure(),
                        &error,
                    ));
                }
            }
        }

        let mut environment = EnvironmentDelta::default();
        if let Some(shellenv) = &config.shellenv() {
            let operation = "shellenv".to_owned();
            let args =
                self.expand_args(provider_id, &operation, shellenv.argv(), &provider_context)?;
            let result = self.execute(CommandExecution {
                provider: provider_id,
                operation: &operation,
                executable,
                args,
                context: &provider_context,
                timeout_ms: shellenv.timeout_ms(),
                identity,
            });
            match result {
                Ok(output) if output.succeeded() => {
                    let text = String::from_utf8(output.stdout().to_vec()).map_err(|source| {
                        ProviderRuntimeError::OutputUtf8 {
                            provider: provider_id.to_owned(),
                            operation: "shellenv".to_owned(),
                            source,
                        }
                    });
                    match text.and_then(|text| {
                        EnvironmentDelta::parse_output(shellenv.format(), &text).map_err(|source| {
                            ProviderRuntimeError::OutputRejected {
                                provider: provider_id.to_owned(),
                                operation: "shellenv".to_owned(),
                                source,
                            }
                        })
                    }) {
                        Ok(entries) => environment.apply_entries(entries),
                        Err(error) => {
                            if shellenv.failure() == FailurePolicy::Error {
                                return Err(error);
                            }
                            diagnostics.push(ProviderDiagnostic::ShellenvRejected {
                                provider: provider_id.to_owned(),
                                policy: shellenv.failure(),
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
                    if shellenv.failure() == FailurePolicy::Error {
                        return Err(error);
                    }
                    diagnostics.push(shellenv_diagnostic(provider_id, shellenv.failure(), &error));
                }
                Err(error) => {
                    if shellenv.failure() == FailurePolicy::Error {
                        return Err(error);
                    }
                    diagnostics.push(shellenv_diagnostic(provider_id, shellenv.failure(), &error));
                }
            }
        }

        let receipt = if prepared_steps == 0 {
            None
        } else {
            Some(ProviderReceipt {
                provider: provider_id.to_owned(),
                config_fingerprint: if context.config_fingerprint() == [0; 32] {
                    ProviderFingerprints::provider_config(config).map_err(|source| {
                        ProviderRuntimeError::Fingerprint {
                            provider: provider_id.to_owned(),
                            source,
                        }
                    })?
                } else {
                    context.config_fingerprint()
                },
                workspace_fingerprint: ProviderFingerprints::workspace(
                    context.workspace(),
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

        Ok(ProviderRunResult::new(ProviderRunResultParts {
            provider: provider_id.to_owned(),
            sensitivity: config.sensitivity(),
            detection,
            prepared_steps,
            environment,
            diagnostics,
            receipt,
        }))
    }

    fn expand_args(
        &self,
        provider: &str,
        operation: &str,
        args: &[String],
        context: &ProviderContext,
    ) -> Result<Vec<String>, ProviderRuntimeError> {
        ProviderTemplate::expand_argv(args, provider, context).map_err(|source| {
            ProviderRuntimeError::Template {
                provider: provider.to_owned(),
                operation: operation.to_owned(),
                source,
            }
        })
    }

    fn execute(
        &self,
        execution: CommandExecution<'_>,
    ) -> Result<CommandOutput, ProviderRuntimeError> {
        let mut environment = execution.context.environment().clone();
        if let Some(identity) = execution.identity {
            environment.insert(
                "HOME".to_owned(),
                identity.home().to_string_lossy().into_owned(),
            );
            environment.insert("USER".to_owned(), identity.user().to_owned());
            environment.insert("LOGNAME".to_owned(), identity.user().to_owned());
        }
        let request = CommandRequest::new(
            execution.executable.display().to_string(),
            execution.context.cwd(),
        )
        .with_args(execution.args)
        .with_environment(environment)
        .with_timeout(Some(
            execution
                .timeout_ms
                .map(Duration::from_millis)
                .unwrap_or(self.command_timeout),
        ))
        .with_output_limits(self.max_stdout_bytes, self.max_stderr_bytes);
        self.executor
            .execute_with_identity(&request, execution.identity)
            .map_err(|source| ProviderRuntimeError::Command {
                provider: execution.provider.to_owned(),
                operation: execution.operation.to_owned(),
                source,
            })
    }
}

fn condition_matches(
    step: &PrepareStep,
    context: &ProviderContext,
) -> Result<bool, ProviderRuntimeError> {
    let Some(condition) = step.when() else {
        return Ok(true);
    };
    match Condition::parse(condition) {
        Ok(condition) => Ok(ConditionEvaluator::evaluate(&condition, context)),
        Err(source) => Err(ProviderRuntimeError::Model(ModelError::InvalidCondition {
            location: "provider prepare condition".to_owned(),
            reason: source,
        })),
    }
}

fn prepare_diagnostic(
    provider: &str,
    step: usize,
    policy: FailurePolicy,
    error: &ProviderRuntimeError,
) -> ProviderDiagnostic {
    let failure = command_failure_parts(error);
    ProviderDiagnostic::PrepareFailed {
        provider: provider.to_owned(),
        step,
        policy,
        status: failure.status,
        timed_out: failure.timed_out,
        stderr: failure.stderr,
    }
}

fn shellenv_diagnostic(
    provider: &str,
    policy: FailurePolicy,
    error: &ProviderRuntimeError,
) -> ProviderDiagnostic {
    let failure = command_failure_parts(error);
    ProviderDiagnostic::ShellenvFailed {
        provider: provider.to_owned(),
        policy,
        status: failure.status,
        timed_out: failure.timed_out,
        stderr: failure.stderr,
    }
}

fn command_failure_parts(error: &ProviderRuntimeError) -> CommandFailureInfo {
    match error {
        ProviderRuntimeError::CommandFailed { output, .. } => CommandFailureInfo {
            status: output.status(),
            timed_out: output.timed_out(),
            stderr: output.stderr().to_vec(),
        },
        ProviderRuntimeError::Command { source, .. } => match source {
            CommandError::Spawn { .. }
            | CommandError::Wait { .. }
            | CommandError::Read { .. }
            | CommandError::ReaderPanicked { .. }
            | CommandError::Kill { .. }
            | CommandError::InvalidProgram
            | CommandError::InvalidArgument
            | CommandError::InvalidEnvironment
            | CommandError::InvalidTimeout
            | CommandError::WorkerUnavailable { .. }
            | CommandError::WorkerSpawn { .. }
            | CommandError::Worker { .. }
            | CommandError::Protocol { .. }
            | CommandError::SupervisorStopped => CommandFailureInfo {
                status: None,
                timed_out: false,
                stderr: Vec::new(),
            },
        },
        _ => CommandFailureInfo {
            status: None,
            timed_out: false,
            stderr: Vec::new(),
        },
    }
}

impl ProviderSupervisor for ProviderRunner {
    fn prepare(&self, job: &ProviderJob) -> Result<ProviderRunResult, ProviderRuntimeError> {
        self.run_with_identity(
            job.provider_id(),
            job.config(),
            job.context(),
            Some(job.identity()),
        )
    }
}
