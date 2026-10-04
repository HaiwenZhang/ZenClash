//! Process-local coordination of persistent writes and whole-root restores.

use std::{
    collections::HashMap,
    fmt,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::{Condvar, Mutex};

#[derive(Default)]
struct Coordinator {
    state: Mutex<LeaseState>,
    changed: Condvar,
}

#[derive(Default)]
struct LeaseState {
    sequence: u64,
    active: Vec<LeaseRecord>,
    waiting_restores: Vec<LeaseRecord>,
}

struct LeaseRecord {
    id: u64,
    paths: Vec<PathBuf>,
    exclusive: bool,
}

struct LeaseInner {
    id: u64,
    paths: Vec<PathBuf>,
}

impl Drop for LeaseInner {
    fn drop(&mut self) {
        let coordinator = coordinator();
        coordinator
            .state
            .lock()
            .active
            .retain(|lease| lease.id != self.id);
        coordinator.changed.notify_all();
    }
}

/// An owned lease, movable between blocking workers and asynchronous owners.
/// Acquire before any store transaction or core-session transition lock.
pub(crate) struct DataWriteLease {
    inner: Arc<LeaseInner>,
    valid: Arc<AtomicBool>,
}

impl fmt::Debug for DataWriteLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DataWriteLease")
            .field("paths", &self.inner.paths)
            .finish()
    }
}

impl Drop for DataWriteLease {
    fn drop(&mut self) {
        self.valid.store(false, Ordering::Release);
    }
}

impl DataWriteLease {
    pub(crate) fn shared(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        acquire(paths, false)
    }

    pub(crate) fn exclusive(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        acquire(paths, true)
    }

    pub(crate) async fn shared_async(paths: Vec<PathBuf>) -> Result<Self, tokio::task::JoinError> {
        tokio::task::spawn_blocking(move || Self::shared(paths)).await
    }

    fn permit(&self) -> WritePermit {
        WritePermit {
            inner: Arc::downgrade(&self.inner),
            valid: Arc::downgrade(&self.valid),
        }
    }

    pub(crate) fn covers(&self, path: &Path) -> bool {
        covers(&self.inner.paths, path)
    }
}

#[derive(Clone)]
struct WritePermit {
    inner: Weak<LeaseInner>,
    valid: Weak<AtomicBool>,
}

impl WritePermit {
    fn acquire(&self, path: &Path) -> Option<DataWriteLease> {
        let valid = self.valid.upgrade()?;
        if !valid.load(Ordering::Acquire) {
            return None;
        }
        let inner = self.inner.upgrade()?;
        if !covers(&inner.paths, path) || !valid.load(Ordering::Acquire) {
            return None;
        }
        // An admitted operation can finish after its parent future is cancelled.
        // Its own authority expires with this guard; escaped parent permits expire
        // immediately when the parent ends, even while this guard retains the scope.
        Some(DataWriteLease {
            inner,
            valid: Arc::new(AtomicBool::new(true)),
        })
    }
}

/// One store's write scope and optional, explicitly borrowed restore authority.
#[derive(Clone)]
pub(crate) struct DataWriteAccess {
    path: PathBuf,
    paths: Vec<PathBuf>,
    permit: Option<WritePermit>,
}

impl fmt::Debug for DataWriteAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DataWriteAccess")
            .field("path", &self.path)
            .finish()
    }
}

impl DataWriteAccess {
    pub(crate) fn new(path: &Path) -> Self {
        Self::for_store(path, &[])
    }

    pub(crate) fn for_store(path: &Path, children: &[&str]) -> Self {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let mut paths = vec![path.clone()];
        paths.extend(children.iter().map(|child| path.join(child)));
        Self {
            // Keep the destination identity: atomic rename writes this logical
            // path, even when its existing final component is a symlink.
            path,
            paths,
            permit: None,
        }
    }

    pub(crate) fn authorized(&self, lease: &DataWriteLease) -> Self {
        assert!(
            self.paths.iter().all(|path| lease.covers(path)),
            "write lease does not cover the store"
        );
        Self {
            path: self.path.clone(),
            paths: self.paths.clone(),
            permit: Some(lease.permit()),
        }
    }

    /// Wait only on a blocking worker, never on GPUI's foreground thread.
    pub(crate) fn acquire(&self) -> DataWriteLease {
        if let Some(lease) = self
            .borrowed_authority(&self.paths)
            .expect("store write authority")
        {
            return lease;
        }
        DataWriteLease::shared(self.paths.clone())
    }

    pub(crate) fn borrowed_authority(
        &self,
        paths: &[PathBuf],
    ) -> Result<Option<DataWriteLease>, String> {
        let lease = self
            .permit
            .as_ref()
            .and_then(|permit| permit.acquire(&self.path));
        let Some(lease) = lease else {
            return Ok(None);
        };
        if paths.iter().all(|path| lease.covers(path)) {
            Ok(Some(lease))
        } else {
            Err("existing write authority does not cover all runtime scopes".into())
        }
    }

    pub(crate) fn acquire_paths(&self, mut paths: Vec<PathBuf>) -> Result<DataWriteLease, String> {
        paths.extend(self.paths.iter().cloned());
        if let Some(lease) = self.borrowed_authority(&paths)? {
            return Ok(lease);
        }
        Ok(DataWriteLease::shared(paths))
    }

    /// Acquire all scopes together; a live borrowed permit must already cover them.
    /// Restore callers reserve external runtime scopes before activating any data.
    pub(crate) async fn acquire_paths_async(
        &self,
        mut paths: Vec<PathBuf>,
    ) -> Result<DataWriteLease, String> {
        paths.extend(self.paths.iter().cloned());
        if let Some(lease) = self.borrowed_authority(&paths)? {
            return Ok(lease);
        }
        DataWriteLease::shared_async(paths)
            .await
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn acquire_async(&self) -> Result<DataWriteLease, tokio::task::JoinError> {
        let access = self.clone();
        tokio::task::spawn_blocking(move || access.acquire()).await
    }
}

fn coordinator() -> &'static Coordinator {
    static COORDINATOR: OnceLock<Coordinator> = OnceLock::new();
    COORDINATOR.get_or_init(Coordinator::default)
}

fn acquire(paths: impl IntoIterator<Item = PathBuf>, exclusive: bool) -> DataWriteLease {
    let requested = paths.into_iter().collect::<Vec<_>>();
    let mut paths = write_paths(&requested);
    let coordinator = coordinator();
    let mut state = coordinator.state.lock();
    state.sequence += 1;
    let id = state.sequence;
    if exclusive {
        state.waiting_restores.push(LeaseRecord {
            id,
            paths: paths.clone(),
            exclusive,
        });
    }
    loop {
        let active_conflict = state
            .active
            .iter()
            .any(|lease| (exclusive || lease.exclusive) && overlapping(&paths, &lease.paths));
        let restore_precedes = state
            .waiting_restores
            .iter()
            .any(|lease| (!exclusive || lease.id < id) && overlapping(&paths, &lease.paths));
        if !active_conflict && !restore_precedes {
            break;
        }
        coordinator.changed.wait(&mut state);
        // A preceding restore may have replaced a symlinked directory while
        // this operation waited. Resolve its physical destination again before
        // admitting it, retaining the stable logical scope throughout the wait.
        paths = write_paths(&requested);
        if let Some(waiting) = state
            .waiting_restores
            .iter_mut()
            .find(|lease| lease.id == id)
        {
            waiting.paths.clone_from(&paths);
        }
    }
    state.waiting_restores.retain(|lease| lease.id != id);
    state.active.push(LeaseRecord {
        id,
        paths: paths.clone(),
        exclusive,
    });
    DataWriteLease {
        inner: Arc::new(LeaseInner { id, paths }),
        valid: Arc::new(AtomicBool::new(true)),
    }
}

#[cfg(test)]
pub(crate) fn has_waiting_restore(path: &Path) -> bool {
    let paths = write_paths(&[path.to_path_buf()]);
    coordinator()
        .state
        .lock()
        .waiting_restores
        .iter()
        .any(|record| overlapping(&paths, &record.paths))
}

fn write_paths(requested: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = requested
        .iter()
        .flat_map(|path| coordinated_paths(path))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn overlapping(first: &[PathBuf], second: &[PathBuf]) -> bool {
    first.iter().any(|first| {
        second
            .iter()
            .any(|second| first.starts_with(second) || second.starts_with(first))
    })
}

/// Independent handles to the same store share the complete read/modify/write lock.
pub(crate) fn shared_transaction(path: &Path) -> Arc<Mutex<()>> {
    static TRANSACTIONS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let paths = coordinated_paths(path);
    let mut transactions = TRANSACTIONS.get_or_init(Mutex::default).lock();
    transactions.retain(|_, transaction| transaction.strong_count() > 0);
    // Prefer the logical key, which survives a restore replacing a symlinked
    // store directory. Also register the physical alias for independent handles.
    let transaction = paths
        .iter()
        .find_map(|path| transactions.get(path).and_then(Weak::upgrade))
        .unwrap_or_else(|| Arc::new(Mutex::new(())));
    for path in paths {
        transactions.insert(path, Arc::downgrade(&transaction));
    }
    transaction
}

pub(crate) fn path_within(path: &Path, root: &Path) -> bool {
    let path = coordinated_paths(path);
    let root = coordinated_paths(root);
    path[0].starts_with(&root[0]) || path[1].starts_with(&root[1])
}

fn normalized_path(path: &Path) -> PathBuf {
    normalized_path_with_limit(path, 40)
}

fn normalized_path_with_limit(path: &Path, links_remaining: usize) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut ancestor = absolute.as_path();
    let mut missing = Vec::new();
    let mut resolved = std::fs::canonicalize(ancestor);
    while resolved.is_err() {
        // A restore temporarily removes directories. A dangling alias still
        // identifies that destination and must keep conflicting with its lease.
        if links_remaining > 0
            && let Ok(target) = std::fs::read_link(ancestor)
        {
            let mut destination = if target.is_absolute() {
                target
            } else {
                ancestor.parent().unwrap_or(Path::new("/")).join(target)
            };
            for component in missing.into_iter().rev() {
                destination.push(component);
            }
            return normalized_path_with_limit(&destination, links_remaining - 1);
        }
        let Some(name) = ancestor.file_name() else {
            break;
        };
        missing.push(name.to_owned());
        let Some(parent) = ancestor.parent() else {
            break;
        };
        ancestor = parent;
        resolved = std::fs::canonicalize(ancestor);
    }
    // Reuse this observation: a second lookup can fail during a restore's
    // rename and discard the physical destination that just resolved correctly.
    let mut normalized = resolved.unwrap_or_else(|_| ancestor.to_path_buf());
    for component in missing.into_iter().rev() {
        normalized.push(component);
    }
    lexical_path(&normalized)
}

fn coordinated_paths(path: &Path) -> [PathBuf; 2] {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    [lexical_path(&absolute), normalized_path(&absolute)]
}

fn covers(scopes: &[PathBuf], path: &Path) -> bool {
    coordinated_paths(path)
        .iter()
        .all(|path| scopes.iter().any(|scope| path.starts_with(scope)))
}

fn lexical_path(path: &Path) -> PathBuf {
    let mut lexical = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                lexical.pop();
            }
            component => lexical.push(component.as_os_str()),
        }
    }
    lexical
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::mpsc,
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    #[cfg(unix)]
    #[test]
    fn a_dangling_directory_alias_still_waits_for_its_destinations_restore() {
        use std::os::unix::fs::symlink;

        let root = test_root("dangling-directory-alias");
        let data = root.join("data");
        let destination = root.join("destination");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(destination.join("files")).unwrap();
        symlink("../destination/files", data.join("files")).unwrap();
        let access = DataWriteAccess::for_store(&data, &["files"]);
        let restore = DataWriteLease::exclusive([destination.clone()]);
        std::fs::remove_dir_all(destination.join("files")).unwrap();
        let (admitted_tx, admitted_rx) = mpsc::channel();
        let writer = thread::spawn(move || {
            let _lease = access.acquire();
            admitted_tx.send(()).unwrap();
        });
        let early = admitted_rx.recv_timeout(Duration::from_millis(100));
        std::fs::create_dir(destination.join("files")).unwrap();
        drop(restore);
        if early.is_err() {
            admitted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        writer.join().unwrap();
        assert!(
            matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
            "dangling directory alias lost its active physical restore scope"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_alias_remains_blocked_while_its_restore_destination_is_replaced() {
        use std::os::unix::fs::symlink;

        let root = test_root("replaced-directory-alias");
        let data = root.join("data");
        let destination = root.join("destination");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(destination.join("files")).unwrap();
        symlink("../destination/files", data.join("files")).unwrap();
        let access = DataWriteAccess::for_store(&data, &["files"]);
        let restore = DataWriteLease::exclusive([destination.clone()]);
        let (admitted_tx, admitted_rx) = mpsc::channel();
        let writer = thread::spawn(move || {
            let _lease = access.acquire();
            admitted_tx.send(()).unwrap();
        });
        let mut early = admitted_rx.recv_timeout(Duration::from_millis(100)).ok();
        for _ in 0..1000 {
            std::fs::rename(destination.join("files"), root.join("parked")).unwrap();
            drop(DataWriteLease::shared([root.join("unrelated")]));
            std::fs::rename(root.join("parked"), destination.join("files")).unwrap();
            drop(DataWriteLease::shared([root.join("unrelated")]));
            early = early.or_else(|| admitted_rx.try_recv().ok());
        }
        drop(restore);
        if early.is_none() {
            admitted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        writer.join().unwrap();
        assert!(
            early.is_none(),
            "directory replacement lost the alias's active physical restore scope"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_waiting_writer_rechecks_the_physical_scope_after_a_restore_replaces_its_symlink() {
        use std::os::unix::fs::symlink;

        let root = test_root("waiting-symlink");
        let data = root.join("data");
        let first = root.join("first");
        let second = root.join("second");
        for directory in [&data, &first, &second] {
            std::fs::create_dir_all(directory).unwrap();
        }
        let logical = data.join("store");
        symlink(&first, &logical).unwrap();
        let access = DataWriteAccess::new(&logical);
        let restoring_data = DataWriteLease::exclusive([data]);
        let restoring_destination = DataWriteLease::exclusive([second.clone()]);
        let (started_tx, started_rx) = mpsc::channel();
        let (written_tx, written_rx) = mpsc::channel();
        let destination = logical.join("completed");
        let writer = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let _lease = access.acquire();
            std::fs::write(destination, b"committed").unwrap();
            written_tx.send(()).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let before_replace = written_rx.recv_timeout(Duration::from_millis(100));
        std::fs::remove_file(&logical).unwrap();
        symlink(&second, &logical).unwrap();
        drop(restoring_data);
        let after_replace = written_rx.recv_timeout(Duration::from_millis(100));
        drop(restoring_destination);
        if before_replace.is_err() && after_replace.is_err() {
            written_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        writer.join().unwrap();
        assert!(matches!(
            before_replace,
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(
            matches!(after_replace, Err(mpsc::RecvTimeoutError::Timeout)),
            "writer used its stale physical scope after the root restore ended"
        );
        assert_eq!(
            std::fs::read(second.join("completed")).unwrap(),
            b"committed"
        );
        assert!(!first.join("completed").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_admitted_writer_finishes_after_parent_cancellation_with_a_restore_waiting() {
        let root = test_root("admitted-write");
        std::fs::create_dir_all(&root).unwrap();
        let parent = DataWriteLease::shared([root.clone()]);
        let access = DataWriteAccess::new(&root).authorized(&parent);
        let admitted = access.acquire();
        let writer_access = access.authorized(&admitted);
        let (restore_started, started) = mpsc::channel();
        let (restored, restored_rx) = mpsc::channel();
        let restore_root = root.clone();
        let restore = thread::spawn(move || {
            restore_started.send(()).unwrap();
            let _lease = DataWriteLease::exclusive([restore_root]);
            restored.send(()).unwrap();
        });
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            restored_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(parent);
        let (written, written_rx) = mpsc::channel();
        let path = root.join("completed");
        let writer = thread::spawn(move || {
            let _admitted = admitted;
            let _nested = writer_access.acquire();
            std::fs::write(path, b"committed").unwrap();
            written.send(()).unwrap();
        });
        written_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        restored_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        writer.join().unwrap();
        restore.join().unwrap();
        assert_eq!(std::fs::read(root.join("completed")).unwrap(), b"committed");
        std::fs::remove_dir_all(root).unwrap();
    }

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "zenclash-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
