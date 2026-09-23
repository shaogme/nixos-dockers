use crate::protocol::{
    read_message, write_message, BackendError, BackendStatus, ClientMessage, ClientRequest,
    PreparedHandoff, ProtocolError, ServerMessage, ServerResponse, PROTOCOL_VERSION,
};
use crate::socket::{peer_credentials, validate_socket_path};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct BackendClient {
    socket_path: PathBuf,
    timeout: Duration,
}

#[derive(Debug)]
pub enum BackendClientError {
    Io(io::Error),
    Protocol(ProtocolError),
    Backend(BackendError),
    UnexpectedResponse,
    TimedOut,
}

impl std::fmt::Display for BackendClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "backend connection failed: {error}"),
            Self::Protocol(error) => error.fmt(f),
            Self::Backend(error) => write!(f, "backend {}: {}", error.class, error.message),
            Self::UnexpectedResponse => f.write_str("backend returned an unexpected response"),
            Self::TimedOut => f.write_str("backend did not become ready before the timeout"),
        }
    }
}

impl std::error::Error for BackendClientError {}

impl From<io::Error> for BackendClientError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ProtocolError> for BackendClientError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

impl BackendClient {
    pub fn new(path: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            socket_path: path.into(),
            timeout,
        }
    }

    pub fn status(&self) -> Result<BackendStatus, BackendClientError> {
        match self.request(ClientRequest::Status)? {
            ServerResponse::Status(status) => Ok(status),
            ServerResponse::Error(error) => Err(BackendClientError::Backend(error)),
            _ => Err(BackendClientError::UnexpectedResponse),
        }
    }

    pub fn plan(&self) -> Result<serde_json::Value, BackendClientError> {
        match self.request(ClientRequest::Plan)? {
            ServerResponse::Plan(plan) => Ok(plan),
            ServerResponse::Error(error) => Err(BackendClientError::Backend(error)),
            _ => Err(BackendClientError::UnexpectedResponse),
        }
    }

    pub fn doctor(&self) -> Result<serde_json::Value, BackendClientError> {
        match self.request(ClientRequest::Doctor)? {
            ServerResponse::Doctor(report) => Ok(report),
            ServerResponse::Error(error) => Err(BackendClientError::Backend(error)),
            _ => Err(BackendClientError::UnexpectedResponse),
        }
    }

    pub fn prepare(
        &self,
        argv: Vec<String>,
        cwd: PathBuf,
        inputs: BTreeMap<String, String>,
        ambient_environment: &BTreeMap<String, String>,
    ) -> Result<PreparedHandoff, BackendClientError> {
        let started = Instant::now();
        let mut delay = Duration::from_millis(10);
        loop {
            let attempt =
                self.prepare_once(argv.clone(), cwd.clone(), &inputs, ambient_environment);
            match attempt {
                Err(BackendClientError::Io(_))
                | Err(BackendClientError::Protocol(ProtocolError::Io(_)))
                | Err(BackendClientError::Backend(BackendError {
                    retryable: true, ..
                })) if started.elapsed() < self.timeout => {
                    thread::sleep(delay.min(self.timeout.saturating_sub(started.elapsed())));
                    delay = delay.saturating_mul(2).min(Duration::from_millis(100));
                }
                Err(BackendClientError::Io(error)) if started.elapsed() >= self.timeout => {
                    let _ = error;
                    return Err(BackendClientError::TimedOut);
                }
                Err(BackendClientError::Protocol(ProtocolError::Io(error)))
                    if started.elapsed() >= self.timeout =>
                {
                    let _ = error;
                    return Err(BackendClientError::TimedOut);
                }
                result => return result,
            }
        }
    }

    fn prepare_once(
        &self,
        argv: Vec<String>,
        cwd: PathBuf,
        inputs: &BTreeMap<String, String>,
        ambient_environment: &BTreeMap<String, String>,
    ) -> Result<PreparedHandoff, BackendClientError> {
        let mut stream = self.connect()?;
        let hello_id = request_id();
        write_request(&mut stream, &hello_id, ClientRequest::Hello)?;
        let hello = read_response(&mut stream, &hello_id)?;
        let info = match hello {
            ServerResponse::Hello(info) => info,
            ServerResponse::Error(error) => return Err(BackendClientError::Backend(error)),
            _ => return Err(BackendClientError::UnexpectedResponse),
        };
        if !matches!(info.state, crate::BackendState::Ready) {
            return Err(BackendClientError::Backend(BackendError {
                class: "backend_not_ready".to_owned(),
                retryable: true,
                message: "backend is still starting".to_owned(),
                action_id: None,
                path: None,
            }));
        }

        let allowed_inputs = info.runtime_inputs.into_iter().collect::<BTreeSet<_>>();
        if inputs.keys().any(|name| !allowed_inputs.contains(name)) {
            return Err(BackendClientError::Backend(BackendError {
                class: "invalid_input".to_owned(),
                retryable: false,
                message: "input is not enabled for runtime requests".to_owned(),
                action_id: None,
                path: None,
            }));
        }
        let allowed_environment = info.environment_names.into_iter().collect::<BTreeSet<_>>();
        let environment = ambient_environment
            .iter()
            .filter(|(name, _)| allowed_environment.contains(*name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let exec_id = request_id();
        write_request(
            &mut stream,
            &exec_id,
            ClientRequest::Exec {
                argv,
                cwd,
                inputs: inputs.clone(),
                environment,
            },
        )?;
        match read_response(&mut stream, &exec_id)? {
            ServerResponse::Prepared(prepared) => validate_prepared(prepared),
            ServerResponse::Error(error) => Err(BackendClientError::Backend(error)),
            _ => Err(BackendClientError::UnexpectedResponse),
        }
    }

    fn request(&self, request: ClientRequest) -> Result<ServerResponse, BackendClientError> {
        let request_id = request_id();
        let mut stream = self.connect()?;
        write_request(&mut stream, &request_id, request)?;
        read_response(&mut stream, &request_id)
    }

    fn connect(&self) -> Result<UnixStream, BackendClientError> {
        validate_socket_path(&self.socket_path)?;
        let stream = UnixStream::connect(&self.socket_path)?;
        stream.set_read_timeout(Some(self.timeout.min(Duration::from_secs(5))))?;
        stream.set_write_timeout(Some(self.timeout.min(Duration::from_secs(5))))?;
        let peer = peer_credentials(&stream)?;
        let effective_uid = unsafe { libc::geteuid() };
        let socket_metadata = std::fs::symlink_metadata(&self.socket_path)?;
        if socket_metadata.uid() != peer.uid
            || (effective_uid == 0 && peer.uid != 0)
            || (effective_uid != 0 && peer.uid != 0 && peer.uid != effective_uid)
        {
            return Err(BackendClientError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "backend socket owner does not match its peer credentials",
            )));
        }
        Ok(stream)
    }
}

fn write_request(
    stream: &mut UnixStream,
    request_id: &str,
    request: ClientRequest,
) -> Result<(), BackendClientError> {
    write_message(
        stream,
        &ClientMessage {
            version: PROTOCOL_VERSION,
            request_id: request_id.to_owned(),
            request,
        },
    )?;
    Ok(())
}

fn read_response(
    stream: &mut UnixStream,
    request_id: &str,
) -> Result<ServerResponse, BackendClientError> {
    let response: ServerMessage = read_message(stream)?;
    if response.version != PROTOCOL_VERSION || response.request_id != request_id {
        return Err(BackendClientError::UnexpectedResponse);
    }
    Ok(response.response)
}

fn validate_prepared(prepared: PreparedHandoff) -> Result<PreparedHandoff, BackendClientError> {
    let uid = unsafe { libc::geteuid() };
    let expected_environment = ["HOME", "USER", "LOGNAME"];
    let login_environment_valid = prepared.login_environment.len() == expected_environment.len()
        && expected_environment.iter().all(|name| {
            prepared.login_environment.get(*name).is_some_and(|value| {
                !value.is_empty() && !value.contains('\0') && !value.contains('\n')
            })
        });
    let home_matches = prepared.login_environment.get("HOME").is_some_and(|home| {
        home.starts_with('/') && home == prepared.identity.home.to_string_lossy().as_ref()
    });
    let user_matches = prepared
        .login_environment
        .get("USER")
        .is_some_and(|user| user == &prepared.identity.user);
    let logname_matches = prepared
        .login_environment
        .get("LOGNAME")
        .is_some_and(|user| user == &prepared.identity.user);
    if !prepared.command.program.is_absolute()
        || prepared
            .command
            .args
            .iter()
            .any(|argument| argument.contains('\0'))
        || !prepared.cwd.is_absolute()
        || prepared
            .cwd
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        || !prepared.cwd.is_dir()
        || !login_environment_valid
        || (!prepared.root_service && (!home_matches || !user_matches || !logname_matches))
        || (prepared.root_service
            && (!prepared
                .login_environment
                .get("HOME")
                .is_some_and(|value| value == "/root")
                || !prepared
                    .login_environment
                    .get("USER")
                    .is_some_and(|value| value == "root")
                || !prepared
                    .login_environment
                    .get("LOGNAME")
                    .is_some_and(|value| value == "root")))
        || !prepared.identity.home.is_absolute()
        || prepared.identity.user.contains('\0')
        || prepared.supplemental_groups.len() > 4096
        || (!prepared.root_service
            && !prepared
                .supplemental_groups
                .contains(&prepared.identity.gid))
        || prepared
            .supplemental_groups
            .windows(2)
            .any(|groups| groups[0] >= groups[1])
        || (uid != 0 && (prepared.identity.uid != uid || prepared.identity.run_as_root))
        || (uid != 0 && prepared.root_service)
        || prepared.receipt_summary.action_count > 65_536
        || prepared.receipt_summary.warning_count > prepared.receipt_summary.action_count
    {
        return Err(BackendClientError::UnexpectedResponse);
    }
    Ok(prepared)
}

fn request_id() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{}-{timestamp}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::{BackendClient, BackendClientError};
    use std::collections::BTreeMap;
    use std::os::unix::net::UnixListener;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn prepare_times_out_while_backend_socket_closes_connections() {
        let temp = TempDir::new().unwrap();
        let socket = temp.path().join("backend.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let worker = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                drop(stream);
            }
        });

        let client = BackendClient::new(&socket, Duration::from_millis(80));
        let result = client.prepare(
            vec!["/bin/true".to_owned()],
            temp.path().to_path_buf(),
            BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(matches!(result, Err(BackendClientError::TimedOut)));
        worker.join().unwrap();
    }
}
