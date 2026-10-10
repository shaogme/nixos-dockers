use super::{
    identity::BackendIdentity,
    lifecycle::BackendSupervisor,
    runtime::{BackendRuntime, RuntimeErrors},
};
use container_init_core::{PlanExecutor, ResolvedIdentity, RuntimeContext};
use container_init_protocol::{
    BackendError, BackendState, CommitResult as WireCommitResult, ExecTransactionState,
    HandoffCommand as WireHandoffCommand, PeerCredentials, PreparedHandoff as WirePreparedHandoff,
    ReceiptSummary, ServerResponse,
};
use libc::geteuid;
use std::{
    collections::BTreeMap,
    fmt::Write as FmtWrite,
    fs::File,
    io::{self, Read},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

const PREPARE_TTL: Duration = Duration::from_secs(30);
const MAX_TRANSACTIONS: usize = 256;
const MAX_TRANSACTIONS_PER_PEER: usize = 8;

pub(super) struct ExecTransaction {
    owner_uid: u32,
    owner_gid: u32,
    snapshot_id: String,
    deadline: Instant,
    state: ExecTransactionStateData,
}

enum ExecTransactionStateData {
    Prepared {
        context: RuntimeContext,
        identity: ResolvedIdentity,
    },
    Committing,
    Committed(ReceiptSummary),
    Aborted,
    Failed(BackendError),
}

pub(super) struct PrepareExecOptions {
    pub(super) argv: Vec<String>,
    pub(super) cwd: PathBuf,
    pub(super) inputs: BTreeMap<String, String>,
    pub(super) environment: BTreeMap<String, String>,
    pub(super) request_budget: Duration,
}

pub(super) struct TransactionService;

impl TransactionService {
    pub(super) fn prepare(
        runtime: &BackendRuntime,
        peer: PeerCredentials,
        options: PrepareExecOptions,
    ) -> Result<WirePreparedHandoff, BackendError> {
        prepare_exec(runtime, peer, options)
    }

    pub(super) fn commit(
        runtime: &BackendRuntime,
        peer: PeerCredentials,
        prepare_id: &str,
    ) -> ServerResponse {
        commit_exec(runtime, peer, prepare_id)
    }

    pub(super) fn abort(
        runtime: &BackendRuntime,
        peer: PeerCredentials,
        prepare_id: &str,
    ) -> ServerResponse {
        abort_exec(runtime, peer, prepare_id)
    }

    pub(super) fn result(
        runtime: &BackendRuntime,
        peer: PeerCredentials,
        prepare_id: &str,
    ) -> ServerResponse {
        get_exec_result(runtime, peer, prepare_id)
    }
}

fn prepare_exec(
    runtime: &BackendRuntime,
    peer: PeerCredentials,
    options: PrepareExecOptions,
) -> Result<WirePreparedHandoff, BackendError> {
    let PrepareExecOptions {
        argv,
        cwd,
        inputs,
        environment,
        request_budget,
    } = options;
    let request_deadline = Instant::now()
        .checked_add(request_budget.min(PREPARE_TTL))
        .ok_or_else(|| {
            RuntimeErrors::backend("invalid_request", false, "request deadline is invalid")
        })?;
    if runtime.status().state != BackendState::Ready {
        return Err(RuntimeErrors::backend(
            "backend_not_ready",
            true,
            "backend is not ready to prepare commands",
        ));
    }
    if argv.len() > 256 {
        return Err(RuntimeErrors::backend(
            "invalid_request",
            false,
            "too many argv fields",
        ));
    }
    if inputs
        .keys()
        .any(|name| !runtime.allows_runtime_input(name))
    {
        return Err(RuntimeErrors::backend(
            "invalid_input",
            false,
            "request contains an undeclared or non-runtime input",
        ));
    }
    if environment
        .keys()
        .any(|name| !runtime.allows_environment_name(name))
    {
        return Err(RuntimeErrors::backend(
            "invalid_environment",
            false,
            "request contains an environment name not used by identity reconciliation",
        ));
    }
    if inputs
        .values()
        .chain(environment.values())
        .any(|value| value.contains('\0'))
    {
        return Err(RuntimeErrors::backend(
            "invalid_request",
            false,
            "runtime values may not contain NUL bytes",
        ));
    }
    let resolved_cwd = BackendIdentity::resolve_cwd_object(&cwd).map_err(|_| {
        RuntimeErrors::backend(
            "invalid_cwd",
            false,
            "request working directory cannot be resolved as a directory",
        )
    })?;
    BackendIdentity::validate_inputs(runtime.request_config(), &inputs)?;
    let mut context =
        RuntimeContext::new(resolved_cwd.canonical_path.clone()).with_environment(environment);
    for (name, value) in inputs {
        context = context.with_cli_input(name, value);
    }
    let executor = PlanExecutor::with_shared_config(Arc::clone(runtime.request_config()), context)
        .with_options(runtime.execution_options().clone());
    let identity = executor
        .resolve_identity_prevalidated()
        .map_err(|error| RuntimeErrors::core(&error))?;
    // 请求始终是客户端 handoff；root_service 仅用于 container-init 启动 backend/sshd 的初始 handoff。
    let root_service =
        BackendSupervisor::is_root_service(runtime.config(), &argv, unsafe { geteuid() }, false);
    if root_service && peer.uid != 0 {
        return Err(RuntimeErrors::backend(
            "permission",
            false,
            "root service handoff requires a root peer",
        ));
    }
    if peer.uid != 0
        && (identity.uid != peer.uid || identity.gid != peer.gid || identity.run_as_root)
    {
        return Err(RuntimeErrors::backend(
            "permission",
            false,
            "non-root peers may only hand off with their current UID and GID",
        ));
    }
    let supplemental_groups = if root_service {
        Vec::new()
    } else {
        BackendIdentity::supplementary_groups(&identity)
            .map_err(|_| {
                RuntimeErrors::backend("identity", false, "supplementary groups unavailable")
            })?
            .into_iter()
            .map(u32::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| {
                RuntimeErrors::backend("identity", false, "supplementary group is out of range")
            })?
    };
    if peer.uid != 0 {
        let observed = BackendIdentity::peer_groups(peer.pid, peer.gid).map_err(|_| {
            RuntimeErrors::backend(
                "permission",
                false,
                "could not verify peer supplementary groups",
            )
        })?;
        if observed != supplemental_groups {
            return Err(RuntimeErrors::backend(
                "permission",
                false,
                "non-root peers may only hand off with their current supplementary groups",
            ));
        }
    }
    let command = executor
        .build_handoff_command(&argv)
        .map_err(|error| RuntimeErrors::core(&error))?;
    if !command.program.is_absolute() || command.args.iter().any(|argument| argument.contains('\0'))
    {
        return Err(RuntimeErrors::backend(
            "invalid_snapshot",
            false,
            "snapshot produced an invalid handoff command",
        ));
    }
    let login_environment = BTreeMap::from([
        (
            "HOME".to_owned(),
            if root_service {
                "/root".to_owned()
            } else {
                identity.home.to_string_lossy().into_owned()
            },
        ),
        (
            "USER".to_owned(),
            if root_service {
                "root".to_owned()
            } else {
                identity.user.clone()
            },
        ),
        (
            "LOGNAME".to_owned(),
            if root_service {
                "root".to_owned()
            } else {
                identity.user.clone()
            },
        ),
    ]);
    let prepare_id = new_prepare_id().map_err(|_| {
        RuntimeErrors::backend(
            "internal",
            false,
            "could not generate prepare transaction id",
        )
    })?;
    let now = Instant::now();
    if request_deadline <= now {
        return Err(RuntimeErrors::backend(
            "request_timeout",
            false,
            "request deadline expired before Prepare completed",
        ));
    }
    let deadline = request_deadline;
    let mut transactions = runtime.transactions();
    transactions.retain(|_, transaction| {
        transaction.deadline > now
            || matches!(transaction.state, ExecTransactionStateData::Committing)
    });
    let peer_transactions = transactions
        .values()
        .filter(|transaction| {
            transaction.owner_uid == peer.uid
                && transaction.owner_gid == peer.gid
                && transaction_is_active(transaction)
        })
        .count();
    if transactions.len() >= MAX_TRANSACTIONS || peer_transactions >= MAX_TRANSACTIONS_PER_PEER {
        return Err(RuntimeErrors::backend(
            "capacity",
            true,
            "backend transaction capacity is temporarily exhausted",
        ));
    }
    transactions.insert(
        prepare_id.clone(),
        ExecTransaction {
            owner_uid: peer.uid,
            owner_gid: peer.gid,
            snapshot_id: runtime.snapshot_id().to_owned(),
            deadline,
            state: ExecTransactionStateData::Prepared {
                context: executor.context().clone(),
                identity: identity.clone(),
            },
        },
    );
    drop(transactions);
    Ok(WirePreparedHandoff {
        prepare_id,
        snapshot_id: runtime.snapshot_id().to_owned(),
        command: WireHandoffCommand {
            program: command.program,
            args: command.args,
        },
        cwd: resolved_cwd.canonical_path,
        cwd_object: resolved_cwd.object,
        login_environment,
        identity: BackendIdentity::to_wire_identity(identity),
        supplemental_groups,
        root_service,
    })
}

fn transaction_is_active(transaction: &ExecTransaction) -> bool {
    matches!(
        &transaction.state,
        ExecTransactionStateData::Prepared { .. } | ExecTransactionStateData::Committing
    )
}

fn commit_exec(
    runtime: &BackendRuntime,
    peer: PeerCredentials,
    prepare_id: &str,
) -> ServerResponse {
    let context = {
        let mut transactions = runtime.transactions();
        let Some(transaction) = transactions.get_mut(prepare_id) else {
            return ServerResponse::Error(RuntimeErrors::backend(
                "unknown_transaction",
                false,
                "prepared transaction is unknown or expired",
            ));
        };
        if transaction.owner_uid != peer.uid || transaction.owner_gid != peer.gid {
            return ServerResponse::Error(RuntimeErrors::backend(
                "permission",
                false,
                "transaction credentials do not match the authenticated peer",
            ));
        }
        if transaction.snapshot_id != runtime.snapshot_id() {
            return ServerResponse::Error(RuntimeErrors::backend(
                "invalid_snapshot",
                false,
                "transaction snapshot no longer matches this backend",
            ));
        }
        match &transaction.state {
            ExecTransactionStateData::Prepared { context, identity }
                if transaction.deadline > Instant::now() =>
            {
                let context = context.clone();
                let identity = identity.clone();
                transaction.state = ExecTransactionStateData::Committing;
                (context, identity)
            }
            ExecTransactionStateData::Prepared { .. } => {
                transaction.state = ExecTransactionStateData::Aborted;
                return ServerResponse::CommitResult(transaction_response(
                    prepare_id,
                    &transaction.state,
                ));
            }
            ExecTransactionStateData::Committing
            | ExecTransactionStateData::Committed(_)
            | ExecTransactionStateData::Aborted
            | ExecTransactionStateData::Failed(_) => {
                return ServerResponse::CommitResult(transaction_response(
                    prepare_id,
                    &transaction.state,
                ));
            }
        }
    };

    let (context, identity) = context;
    let executor = PlanExecutor::with_shared_config(Arc::clone(runtime.request_config()), context)
        .with_options(runtime.execution_options().clone());
    let result = executor
        .execute_prevalidated_with_identity(runtime.request_plan(), &[], &identity)
        .map(|report| ReceiptSummary {
            succeeded: report.succeeded(),
            action_count: report.outcomes.len(),
            warning_count: report.warnings.len(),
        })
        .map_err(|error| RuntimeErrors::core(&error));

    let mut transactions = runtime.transactions();
    let Some(transaction) = transactions.get_mut(prepare_id) else {
        return ServerResponse::Error(RuntimeErrors::backend(
            "unknown_transaction",
            false,
            "transaction disappeared while committing",
        ));
    };
    transaction.state = match result {
        Ok(summary) => ExecTransactionStateData::Committed(summary),
        Err(error) => ExecTransactionStateData::Failed(error),
    };
    ServerResponse::CommitResult(transaction_response(prepare_id, &transaction.state))
}

fn abort_exec(runtime: &BackendRuntime, peer: PeerCredentials, prepare_id: &str) -> ServerResponse {
    let mut transactions = runtime.transactions();
    let Some(transaction) = transactions.get_mut(prepare_id) else {
        return ServerResponse::Error(RuntimeErrors::backend(
            "unknown_transaction",
            false,
            "prepared transaction is unknown or expired",
        ));
    };
    if transaction.owner_uid != peer.uid || transaction.owner_gid != peer.gid {
        return ServerResponse::Error(RuntimeErrors::backend(
            "permission",
            false,
            "transaction credentials do not match the authenticated peer",
        ));
    }
    if matches!(
        &transaction.state,
        ExecTransactionStateData::Prepared { .. }
    ) {
        transaction.state = ExecTransactionStateData::Aborted;
    }
    ServerResponse::CommitResult(transaction_response(prepare_id, &transaction.state))
}

fn get_exec_result(
    runtime: &BackendRuntime,
    peer: PeerCredentials,
    prepare_id: &str,
) -> ServerResponse {
    let mut transactions = runtime.transactions();
    let Some(transaction) = transactions.get_mut(prepare_id) else {
        return ServerResponse::Error(RuntimeErrors::backend(
            "unknown_transaction",
            false,
            "prepared transaction is unknown or expired",
        ));
    };
    if transaction.owner_uid != peer.uid || transaction.owner_gid != peer.gid {
        return ServerResponse::Error(RuntimeErrors::backend(
            "permission",
            false,
            "transaction credentials do not match the authenticated peer",
        ));
    }
    if transaction.deadline <= Instant::now()
        && matches!(
            &transaction.state,
            ExecTransactionStateData::Prepared { .. }
        )
    {
        transaction.state = ExecTransactionStateData::Aborted;
    }
    ServerResponse::CommitResult(transaction_response(prepare_id, &transaction.state))
}

fn transaction_response(prepare_id: &str, state: &ExecTransactionStateData) -> WireCommitResult {
    match state {
        ExecTransactionStateData::Prepared { .. } => WireCommitResult {
            prepare_id: prepare_id.to_owned(),
            state: ExecTransactionState::Prepared,
            receipt_summary: None,
            error: None,
        },
        ExecTransactionStateData::Committing => WireCommitResult {
            prepare_id: prepare_id.to_owned(),
            state: ExecTransactionState::Committing,
            receipt_summary: None,
            error: None,
        },
        ExecTransactionStateData::Committed(summary) => WireCommitResult {
            prepare_id: prepare_id.to_owned(),
            state: ExecTransactionState::Committed,
            receipt_summary: Some(summary.clone()),
            error: None,
        },
        ExecTransactionStateData::Aborted => WireCommitResult {
            prepare_id: prepare_id.to_owned(),
            state: ExecTransactionState::Aborted,
            receipt_summary: None,
            error: None,
        },
        ExecTransactionStateData::Failed(error) => WireCommitResult {
            prepare_id: prepare_id.to_owned(),
            state: ExecTransactionState::Failed,
            receipt_summary: None,
            error: Some(error.clone()),
        },
    }
}

fn new_prepare_id() -> io::Result<String> {
    let mut bytes = [0u8; 24];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(value)
}
