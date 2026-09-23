use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PathScope {
    Exact,
    Subtree,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProcessNamespace {
    User,
    Mount,
    UserAndMount,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ResourceKey {
    Accounts(BTreeSet<PathBuf>),
    Path {
        path: PathBuf,
        scope: PathScope,
    },
    Cgroup {
        hierarchy: PathBuf,
        controllers: BTreeSet<String>,
    },
    ProcessNamespace(ProcessNamespace),
}

impl ResourceKey {
    pub fn accounts(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        Self::Accounts(paths.into_iter().collect())
    }

    pub fn path(path: impl Into<PathBuf>, scope: PathScope) -> Self {
        Self::Path {
            path: path.into(),
            scope,
        }
    }

    pub fn cgroup(
        hierarchy: impl Into<PathBuf>,
        controllers: impl IntoIterator<Item = String>,
    ) -> Self {
        Self::Cgroup {
            hierarchy: hierarchy.into(),
            controllers: controllers.into_iter().collect(),
        }
    }

    fn normalize(self) -> io::Result<Self> {
        match self {
            Self::Accounts(paths) => Ok(Self::Accounts(
                paths
                    .into_iter()
                    .map(|path| normalize_path(&path))
                    .collect::<io::Result<_>>()?,
            )),
            Self::Path { path, scope } => Ok(Self::Path {
                path: normalize_path(&path)?,
                scope,
            }),
            Self::Cgroup {
                hierarchy,
                controllers,
            } => Ok(Self::Cgroup {
                hierarchy: normalize_path(&hierarchy)?,
                controllers,
            }),
            Self::ProcessNamespace(kind) => Ok(Self::ProcessNamespace(kind)),
        }
    }

    fn conflicts(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Accounts(left), Self::Accounts(right)) => {
                left.iter().any(|path| right.contains(path))
            }
            (
                Self::Path {
                    path: left,
                    scope: left_scope,
                },
                Self::Path {
                    path: right,
                    scope: right_scope,
                },
            ) => {
                left == right
                    || (*left_scope == PathScope::Subtree && right.starts_with(left))
                    || (*right_scope == PathScope::Subtree && left.starts_with(right))
            }
            (
                Self::Cgroup {
                    hierarchy: left, ..
                },
                Self::Cgroup {
                    hierarchy: right, ..
                },
            ) => left == right,
            (Self::ProcessNamespace(left), Self::ProcessNamespace(right)) => {
                namespace_mask(*left) & namespace_mask(*right) != 0
            }
            _ => false,
        }
    }
}

fn namespace_mask(namespace: ProcessNamespace) -> u8 {
    match namespace {
        ProcessNamespace::User => 0b01,
        ProcessNamespace::Mount => 0b10,
        ProcessNamespace::UserAndMount => 0b11,
    }
}

#[derive(Clone, Debug, Default)]
pub struct ResourceLockManager {
    inner: Arc<LockInner>,
}

#[derive(Debug, Default)]
struct LockInner {
    state: Mutex<LockState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct LockState {
    next_ticket: u64,
    waiting: VecDeque<(u64, Vec<ResourceKey>)>,
    active: Vec<(u64, Vec<ResourceKey>)>,
}

impl ResourceLockManager {
    pub fn acquire(
        &self,
        keys: impl IntoIterator<Item = ResourceKey>,
    ) -> io::Result<ResourceLockGuard> {
        let keys = keys
            .into_iter()
            .map(ResourceKey::normalize)
            .collect::<io::Result<BTreeSet<_>>>()?
            .into_iter()
            .collect::<Vec<_>>();
        let mut state = lock_state(&self.inner.state);
        let ticket = state.next_ticket;
        state.next_ticket = state.next_ticket.wrapping_add(1);
        state.waiting.push_back((ticket, keys.clone()));

        loop {
            let at_front = state
                .waiting
                .front()
                .is_some_and(|(waiting_ticket, _)| *waiting_ticket == ticket);
            let conflicts = state
                .active
                .iter()
                .any(|(_, active)| sets_conflict(&keys, active));
            if at_front && !conflicts {
                state.waiting.pop_front();
                state.active.push((ticket, keys));
                self.inner.changed.notify_all();
                return Ok(ResourceLockGuard {
                    inner: Arc::clone(&self.inner),
                    ticket,
                });
            }
            state = self
                .inner
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub fn active_count(&self) -> usize {
        lock_state(&self.inner.state).active.len()
    }

    #[cfg(test)]
    fn pending_count(&self) -> usize {
        lock_state(&self.inner.state).waiting.len()
    }
}

#[derive(Debug)]
pub struct ResourceLockGuard {
    inner: Arc<LockInner>,
    ticket: u64,
}

impl Drop for ResourceLockGuard {
    fn drop(&mut self) {
        let mut state = lock_state(&self.inner.state);
        state.active.retain(|(ticket, _)| *ticket != self.ticket);
        self.inner.changed.notify_all();
    }
}

fn sets_conflict(left: &[ResourceKey], right: &[ResourceKey]) -> bool {
    left.iter()
        .any(|left| right.iter().any(|right| left.conflicts(right)))
}

fn normalize_path(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resource paths must be absolute",
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resource paths must not contain parent traversal",
        ));
    }

    let mut normalized = PathBuf::from("/");
    let mut components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_os_string()),
            _ => None,
        })
        .peekable();

    while let Some(component) = components.next() {
        normalized.push(&component);
        if components.peek().is_some() && fs::symlink_metadata(&normalized).is_ok() {
            normalized = fs::canonicalize(&normalized)?;
        }
    }
    Ok(normalized)
}

fn lock_state(state: &Mutex<LockState>) -> MutexGuard<'_, LockState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::{PathScope, ProcessNamespace, ResourceKey, ResourceLockManager};
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    fn exact(path: impl Into<PathBuf>) -> ResourceKey {
        ResourceKey::path(path, PathScope::Exact)
    }

    fn subtree(path: impl Into<PathBuf>) -> ResourceKey {
        ResourceKey::path(path, PathScope::Subtree)
    }

    #[test]
    fn path_locks_use_components_and_subtree_overlap() {
        let temp = TempDir::new().unwrap();
        let base = temp.path();
        let manager = ResourceLockManager::default();
        let held = manager.acquire([subtree(base.join("project"))]).unwrap();

        let independent = manager
            .acquire([exact(base.join("project-old/config"))])
            .unwrap();
        drop(independent);

        let (sent, received) = mpsc::channel();
        let second_manager = manager.clone();
        let child = base.join("project/config");
        let join = thread::spawn(move || {
            let _guard = second_manager.acquire([exact(child)]).unwrap();
            sent.send(()).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        drop(held);
        received.recv_timeout(Duration::from_secs(1)).unwrap();
        join.join().unwrap();
    }

    #[test]
    fn account_sets_and_cgroups_conflict_on_shared_database_or_hierarchy() {
        let temp = TempDir::new().unwrap();
        let passwd = temp.path().join("passwd");
        let group = temp.path().join("group");
        let manager = ResourceLockManager::default();
        let held = manager
            .acquire([ResourceKey::accounts([passwd.clone(), group.clone()])])
            .unwrap();

        let second_manager = manager.clone();
        let (sent, received) = mpsc::channel();
        let join = thread::spawn(move || {
            let _guard = second_manager
                .acquire([ResourceKey::accounts([passwd])])
                .unwrap();
            sent.send(()).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        drop(held);
        received.recv_timeout(Duration::from_secs(1)).unwrap();
        join.join().unwrap();

        let held = manager
            .acquire([ResourceKey::cgroup("/sys/fs/cgroup", ["cpu".into()])])
            .unwrap();
        let second_manager = manager.clone();
        let (sent, received) = mpsc::channel();
        let join = thread::spawn(move || {
            let _guard = second_manager
                .acquire([ResourceKey::cgroup("/sys/fs/cgroup", ["memory".into()])])
                .unwrap();
            sent.send(()).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        drop(held);
        received.recv_timeout(Duration::from_secs(1)).unwrap();
        join.join().unwrap();
    }

    #[test]
    fn disjoint_resources_can_be_held_at_the_same_time() {
        let temp = TempDir::new().unwrap();
        let manager = ResourceLockManager::default();
        let _first = manager.acquire([exact(temp.path().join("a"))]).unwrap();
        let _second = manager.acquire([exact(temp.path().join("b"))]).unwrap();
        assert_eq!(manager.active_count(), 2);
    }

    #[test]
    fn namespace_locks_conflict_when_their_affected_scopes_overlap() {
        let manager = ResourceLockManager::default();
        let _user = manager
            .acquire([ResourceKey::ProcessNamespace(ProcessNamespace::User)])
            .unwrap();
        let (sent, received) = mpsc::channel();
        let second_manager = manager.clone();
        let join = thread::spawn(move || {
            let _guard = second_manager
                .acquire([ResourceKey::ProcessNamespace(
                    ProcessNamespace::UserAndMount,
                )])
                .unwrap();
            sent.send(()).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        drop(_user);
        received.recv_timeout(Duration::from_secs(1)).unwrap();
        join.join().unwrap();

        let _user = manager
            .acquire([ResourceKey::ProcessNamespace(ProcessNamespace::User)])
            .unwrap();
        let _mount = manager
            .acquire([ResourceKey::ProcessNamespace(ProcessNamespace::Mount)])
            .unwrap();
        assert_eq!(manager.active_count(), 2);
    }

    #[test]
    fn requests_are_granted_in_fifo_order_even_when_later_keys_are_free() {
        let temp = TempDir::new().unwrap();
        let manager = ResourceLockManager::default();
        let held = manager.acquire([exact(temp.path().join("a"))]).unwrap();
        let (granted, grants) = mpsc::channel();
        let (release_first, wait_release) = mpsc::channel();
        let first_granted = granted.clone();
        let first_manager = manager.clone();
        let first_path = temp.path().join("a");
        let first = thread::spawn(move || {
            let _guard = first_manager.acquire([exact(first_path)]).unwrap();
            first_granted.send("first").unwrap();
            wait_release.recv().unwrap();
        });
        while manager.pending_count() != 1 {
            thread::yield_now();
        }

        let second_manager = manager.clone();
        let second_granted = granted;
        let second_path = temp.path().join("b");
        let second = thread::spawn(move || {
            let _guard = second_manager.acquire([exact(second_path)]).unwrap();
            second_granted.send("second").unwrap();
        });
        while manager.pending_count() != 2 {
            thread::yield_now();
        }
        assert!(grants.recv_timeout(Duration::from_millis(50)).is_err());

        drop(held);
        assert_eq!(
            grants.recv_timeout(Duration::from_secs(1)).unwrap(),
            "first"
        );
        assert_eq!(
            grants.recv_timeout(Duration::from_secs(1)).unwrap(),
            "second"
        );
        release_first.send(()).unwrap();
        first.join().unwrap();
        second.join().unwrap();
    }

    #[test]
    fn a_panicking_owner_releases_its_entire_lock_set() {
        let temp = TempDir::new().unwrap();
        let manager = ResourceLockManager::default();
        let thread_manager = manager.clone();
        let path = temp.path().join("panic");
        let join = thread::spawn(move || {
            let _guard = thread_manager
                .acquire([exact(path.clone()), subtree(path.parent().unwrap())])
                .unwrap();
            panic!("exercise RAII release");
        });
        assert!(join.join().is_err());
        let _guard = manager.acquire([exact(temp.path().join("panic"))]).unwrap();
        assert_eq!(manager.active_count(), 1);
    }

    #[test]
    fn a_multi_key_request_does_not_deadlock_with_reversed_key_order() {
        let temp = TempDir::new().unwrap();
        let manager = ResourceLockManager::default();
        let first_path = temp.path().join("first");
        let second_path = temp.path().join("second");
        let first_keys = [exact(first_path.clone()), exact(second_path.clone())];
        let second_keys = [exact(second_path), exact(first_path)];
        let first_guard = manager.acquire(first_keys).unwrap();

        let second_manager = manager.clone();
        let (sent, received) = mpsc::channel();
        let second = thread::spawn(move || {
            let _guard = second_manager.acquire(second_keys).unwrap();
            sent.send(()).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        drop(first_guard);
        received.recv_timeout(Duration::from_secs(1)).unwrap();
        second.join().unwrap();
    }
}
