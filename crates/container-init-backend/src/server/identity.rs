use super::runtime::{BackendRuntime, RuntimeErrors};
use container_init_bootstrap_model::BootstrapConfig;
use container_init_core::{
    IdentitySource as CoreIdentitySource, PlanExecutor, ResolvedIdentity, RuntimeContext,
    WorkspaceStatus as CoreWorkspaceStatus,
};
use container_init_protocol::{
    BackendError, BackendState, CwdObject, IdentityRequest, IdentitySource as WireIdentitySource,
    PeerCredentials, PreparedIdentity, ResolvedIdentity as WireResolvedIdentity,
    WorkspaceStatus as WireWorkspaceStatus,
};
use libc::{
    fcntl, getgrouplist, gid_t, open, FD_CLOEXEC, F_GETFD, F_SETFD, O_CLOEXEC, O_DIRECTORY, O_PATH,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    fs::{self, File},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt, net::UnixStream},
    },
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub(super) struct IdentityRequestOptions {
    pub(super) requested: IdentityRequest,
    pub(super) cwd: PathBuf,
    pub(super) inputs: BTreeMap<String, String>,
    pub(super) environment: BTreeMap<String, String>,
}

pub(super) struct ResolvedCwd {
    pub(super) canonical_path: PathBuf,
    pub(super) object: CwdObject,
}

pub(super) struct BackendIdentity;

impl BackendIdentity {
    pub(super) fn prepare(
        runtime: &BackendRuntime,
        peer: PeerCredentials,
        options: IdentityRequestOptions,
    ) -> Result<PreparedIdentity, BackendError> {
        prepare_identity(runtime, peer, options)
    }

    pub(super) fn to_wire_identity(identity: ResolvedIdentity) -> WireResolvedIdentity {
        to_wire_identity(identity)
    }

    pub(super) fn supplementary_groups(identity: &ResolvedIdentity) -> io::Result<Vec<gid_t>> {
        supplementary_groups(identity)
    }

    pub(super) fn resolve_cwd_object(path: &Path) -> io::Result<ResolvedCwd> {
        resolve_cwd_object(path)
    }

    pub(super) fn validate_inputs(
        config: &BootstrapConfig,
        inputs: &BTreeMap<String, String>,
    ) -> Result<(), BackendError> {
        validate_inputs(config, inputs)
    }

    pub(super) fn peer_groups(pid: u32, effective_gid: u32) -> io::Result<Vec<u32>> {
        peer_groups(pid, effective_gid)
    }

    pub(super) fn set_cloexec(stream: &UnixStream) -> io::Result<()> {
        set_cloexec(stream)
    }
}

fn to_wire_identity(identity: ResolvedIdentity) -> WireResolvedIdentity {
    WireResolvedIdentity {
        uid: identity.uid,
        gid: identity.gid,
        user: identity.user,
        home: identity.home,
        run_as_root: identity.run_as_root,
        uid_source: to_wire_identity_source(identity.uid_source),
        gid_source: to_wire_identity_source(identity.gid_source),
        workspace: match identity.workspace {
            CoreWorkspaceStatus::Mounted => WireWorkspaceStatus::Mounted,
            CoreWorkspaceStatus::NotMounted => WireWorkspaceStatus::NotMounted,
            CoreWorkspaceStatus::Unavailable => WireWorkspaceStatus::Unavailable,
        },
    }
}

fn to_wire_identity_source(source: CoreIdentitySource) -> WireIdentitySource {
    match source {
        CoreIdentitySource::RunAsRoot => WireIdentitySource::RunAsRoot,
        CoreIdentitySource::ExplicitHost => WireIdentitySource::ExplicitHost,
        CoreIdentitySource::ExplicitContainer => WireIdentitySource::ExplicitContainer,
        CoreIdentitySource::WorkspaceMount => WireIdentitySource::WorkspaceMount,
        CoreIdentitySource::ProfileDefault => WireIdentitySource::ProfileDefault,
        CoreIdentitySource::Current => WireIdentitySource::Current,
    }
}

fn prepare_identity(
    runtime: &BackendRuntime,
    peer: PeerCredentials,
    options: IdentityRequestOptions,
) -> Result<PreparedIdentity, BackendError> {
    let IdentityRequestOptions {
        requested,
        cwd,
        inputs,
        environment,
    } = options;
    if runtime.status().state != BackendState::Ready {
        return Err(RuntimeErrors::backend(
            "backend_not_ready",
            true,
            "container-init is not ready to prepare an identity",
        ));
    }
    let requested_peer = match &requested {
        IdentityRequest::Peer { uid, gid } => PeerCredentials {
            pid: peer.pid,
            uid: *uid,
            gid: *gid,
        },
        IdentityRequest::Root => PeerCredentials {
            pid: peer.pid,
            uid: 0,
            gid: 0,
        },
        IdentityRequest::User { .. } => peer,
    };
    if peer.uid != 0 && (requested_peer.uid != peer.uid || requested_peer.gid != peer.gid) {
        return Err(RuntimeErrors::backend(
            "permission",
            false,
            "non-root broker peers may only request their observed identity",
        ));
    }
    if matches!(
        requested,
        IdentityRequest::Root | IdentityRequest::User { .. }
    ) && peer.uid != 0
    {
        return Err(RuntimeErrors::backend(
            "permission",
            false,
            "only the root backend may request a named or root identity",
        ));
    }
    if peer.uid == 0 {
        match &requested {
            IdentityRequest::Peer { uid, gid } if !allowed_peer_identity(runtime, *uid, *gid) => {
                return Err(RuntimeErrors::backend(
                    "permission",
                    false,
                    "root broker peer identity is not allowed by the profile",
                ));
            }
            IdentityRequest::User { name }
                if name != "root"
                    && runtime.request_config().identity.default_user.as_deref() != Some(name) =>
            {
                return Err(RuntimeErrors::backend(
                    "permission",
                    false,
                    "named identity is not allowed by the profile",
                ));
            }
            IdentityRequest::Peer { .. } | IdentityRequest::Root | IdentityRequest::User { .. } => {
            }
        }
    }
    let cwd = resolve_cwd_object(&cwd)
        .map(|resolved| resolved.canonical_path)
        .map_err(|_| {
            RuntimeErrors::backend(
                "permission",
                false,
                "identity request working directory is unavailable",
            )
        })?;
    let mut config = runtime.request_config().as_ref().clone();
    let mut inputs = inputs;
    let identity_inputs = [
        config.identity.run_as_root_input.clone(),
        config.identity.uid_input.clone(),
        config.identity.gid_input.clone(),
        config.identity.home_input.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<BTreeSet<_>>();
    inputs.retain(|name, _| !identity_inputs.contains(name));
    let mut environment = environment;
    environment.retain(|name, _| !identity_inputs.contains(name));
    match requested {
        IdentityRequest::Peer { uid, gid } => {
            config.identity.default_user = None;
            config.identity.default_uid = Some(uid);
            config.identity.default_gid = Some(gid);
            config.identity.auto_mapping = false;
        }
        IdentityRequest::Root => {
            if let Some(name) = config.identity.run_as_root_input.clone() {
                inputs.entry(name).or_insert_with(|| "1".to_owned());
            } else {
                config.identity.default_user = Some("root".to_owned());
                config.identity.default_uid = Some(0);
                config.identity.default_gid = Some(0);
                config.identity.auto_mapping = false;
            }
        }
        IdentityRequest::User { name } => {
            config.identity.default_user = Some(name);
            config.identity.default_uid = None;
            config.identity.default_gid = None;
            config.identity.auto_mapping = false;
        }
    }
    validate_inputs(&config, &inputs)?;
    let mut context = RuntimeContext::new(cwd).with_environment(environment);
    for (name, value) in inputs {
        context = context.with_cli_input(name, value);
    }
    let executor = PlanExecutor::with_shared_config(Arc::new(config), context)
        .with_options(runtime.execution_options().clone());
    let report = executor
        .execute_prevalidated(runtime.request_plan().as_ref(), &[])
        .map_err(|error| RuntimeErrors::core(&error))?;
    let groups = supplementary_groups(&report.identity).map_err(|_| {
        RuntimeErrors::backend("identity", false, "supplementary groups unavailable")
    })?;
    let supplemental_groups = groups
        .into_iter()
        .map(u32::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| {
            RuntimeErrors::backend("identity", false, "supplementary group is out of range")
        })?;
    Ok(PreparedIdentity {
        uid: report.identity.uid,
        gid: report.identity.gid,
        user: report.identity.user,
        home: report.identity.home,
        supplemental_groups,
        run_as_root: report.identity.run_as_root,
    })
}

fn allowed_peer_identity(runtime: &BackendRuntime, uid: u32, gid: u32) -> bool {
    let startup = runtime.startup_identity();
    if uid == startup.uid && gid == startup.gid {
        return true;
    }
    runtime
        .request_config()
        .identity
        .default_uid
        .zip(runtime.request_config().identity.default_gid)
        .is_some_and(|(default_uid, default_gid)| uid == default_uid && gid == default_gid)
}

fn set_cloexec(stream: &UnixStream) -> io::Result<()> {
    let fd = stream.as_raw_fd();
    let flags = unsafe { fcntl(fd, F_GETFD) };
    if flags < 0 || unsafe { fcntl(fd, F_SETFD, flags | FD_CLOEXEC) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn validate_inputs(
    config: &BootstrapConfig,
    inputs: &BTreeMap<String, String>,
) -> Result<(), BackendError> {
    let mut targets = BTreeSet::new();
    for (name, value) in inputs {
        let declaration = config
            .inputs
            .get(name)
            .or_else(|| {
                config
                    .inputs
                    .values()
                    .find(|input| input.aliases.iter().any(|alias| alias == name))
            })
            .filter(|input| input.runtime)
            .ok_or_else(|| {
                RuntimeErrors::backend("invalid_input", false, "runtime input is not declared")
            })?;
        if !targets.insert(declaration.target.as_str()) {
            return Err(RuntimeErrors::backend(
                "invalid_input",
                false,
                "request sets an input target more than once",
            ));
        }
        declaration.parse_value(value).map_err(|_| {
            RuntimeErrors::backend("invalid_input", false, "runtime input has an invalid value")
        })?;
    }
    Ok(())
}

fn resolve_cwd_object(path: &Path) -> io::Result<ResolvedCwd> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cwd must be absolute",
        ));
    }
    let path_c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cwd contains a NUL byte"))?;
    let fd = unsafe { open(path_c.as_ptr(), O_PATH | O_DIRECTORY | O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let directory = unsafe { File::from_raw_fd(fd) };
    let metadata = directory.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cwd is not a directory",
        ));
    }
    let canonical_path = fs::read_link(format!("/proc/self/fd/{}", directory.as_raw_fd()))?;
    if !canonical_path.is_absolute() || canonical_path.to_string_lossy().ends_with(" (deleted)") {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "cwd directory changed while it was being resolved",
        ));
    }
    Ok(ResolvedCwd {
        canonical_path,
        object: CwdObject {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
    })
}

fn peer_groups(pid: u32, effective_gid: u32) -> io::Result<Vec<u32>> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    let line = status
        .lines()
        .find(|line| line.starts_with("Groups:"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "peer status lacks Groups"))?;
    let mut groups = BTreeSet::from([effective_gid]);
    for value in line.split_whitespace().skip(1) {
        groups.insert(value.parse::<u32>().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "peer Groups field is malformed")
        })?);
    }
    Ok(groups.into_iter().collect())
}

fn supplementary_groups(identity: &ResolvedIdentity) -> io::Result<Vec<gid_t>> {
    let username = CString::new(identity.user.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "user name contains NUL"))?;
    let mut count = 16_i32;
    loop {
        let mut groups = vec![identity.gid as gid_t; count as usize];
        let result = unsafe {
            getgrouplist(
                username.as_ptr(),
                identity.gid as gid_t,
                groups.as_mut_ptr(),
                &mut count,
            )
        };
        if result >= 0 {
            groups.truncate(count as usize);
            groups.sort_unstable();
            groups.dedup();
            if !groups.contains(&(identity.gid as gid_t)) {
                groups.push(identity.gid as gid_t);
            }
            groups.sort_unstable();
            return Ok(groups);
        }
        if count <= 0 || count > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "supplementary group list is invalid or too large",
            ));
        }
    }
}
