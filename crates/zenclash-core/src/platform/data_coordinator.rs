//! Process-local coordination of persistent writes and whole-root restores.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use parking_lot::{Condvar, Mutex};

const MAX_RECENT_EXCLUSIVE_RELEASES: usize = 256;

#[derive(Default)]
struct Coordinator {
    state: Mutex<LeaseState>,
    changed: Condvar,
    /// Monotonically incremented under `state` whenever an exclusive lease is
    /// released, allowing path resolution outside the mutex to detect
    /// concurrent symlink or directory replacements before lease admission.
    exclusive_releases: AtomicU64,
}

#[derive(Default)]
struct LeaseState {
    sequence: u64,
    active: Vec<LeaseRecord>,
    waiting_restores: Vec<LeaseRecord>,
    recent_exclusive_releases: VecDeque<ReleasedExclusive>,
}

struct ReleasedExclusive {
    epoch: u64,
    paths: Vec<PathBuf>,
}

impl LeaseState {
    fn exclusive_release_overlapped(
        &self,
        observed_epoch: u64,
        current_epoch: u64,
        paths: &[PathBuf],
    ) -> bool {
        if current_epoch == observed_epoch {
            return false;
        }
        let Some(oldest) = self.recent_exclusive_releases.front() else {
            return true;
        };
        if oldest.epoch > observed_epoch.saturating_add(1) {
            return true;
        }
        self.recent_exclusive_releases
            .iter()
            .rev()
            .take_while(|released| released.epoch > observed_epoch)
            .any(|released| overlapping(paths, &released.paths))
    }
}

struct LeaseRecord {
    id: u64,
    paths: Vec<PathBuf>,
    exclusive: bool,
}

struct LeaseInner {
    id: u64,
    paths: Vec<PathBuf>,
    exclusive: bool,
}

impl Drop for LeaseInner {
    fn drop(&mut self) {
        let coordinator = coordinator();
        {
            let mut state = coordinator.state.lock();
            state.active.retain(|lease| lease.id != self.id);
            if self.exclusive {
                let epoch = coordinator
                    .exclusive_releases
                    .fetch_add(1, Ordering::Release)
                    + 1;
                if state.recent_exclusive_releases.len() >= MAX_RECENT_EXCLUSIVE_RELEASES {
                    state.recent_exclusive_releases.pop_front();
                }
                state
                    .recent_exclusive_releases
                    .push_back(ReleasedExclusive {
                        epoch,
                        paths: std::mem::take(&mut self.paths),
                    });
            }
        }
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
    pub(crate) fn scopes(&self) -> &[PathBuf] {
        &self.inner.paths
    }

    pub(crate) fn shared(paths: impl IntoIterator<Item = PathBuf>) -> std::io::Result<Self> {
        let lease = acquire(paths, false);
        // Check after admission: a restore may have failed while this writer waited.
        ensure_writes_available(lease.scopes())?;
        Ok(lease)
    }

    pub(crate) fn exclusive(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        acquire(paths, true)
    }

    pub(crate) async fn shared_async(paths: Vec<PathBuf>) -> std::io::Result<Self> {
        tokio::task::spawn_blocking(move || Self::shared(paths))
            .await
            .map_err(std::io::Error::other)?
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
    /// Unfinished restores reject ordinary writes; a live borrowed restore permit can finish.
    pub(crate) fn acquire(&self) -> std::io::Result<DataWriteLease> {
        if let Some(lease) = self
            .borrowed_authority(&self.paths)
            .map_err(std::io::Error::other)?
        {
            return Ok(lease);
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
        DataWriteLease::shared(paths).map_err(|error| error.to_string())
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

    pub(crate) async fn acquire_async(&self) -> std::io::Result<DataWriteLease> {
        let access = self.clone();
        tokio::task::spawn_blocking(move || access.acquire())
            .await
            .map_err(std::io::Error::other)?
    }
}

/// Publishes memory while no active or waiting root restore overlaps cached read scopes.
/// Keeps the existing coordinator guard through the callback, without registering a lease.
/// Neither acquisition nor release waits or resolves filesystem aliases; callbacks must not
/// acquire data/store/session locks or perform I/O (a caller may try_lock its store transaction).
pub(crate) fn publish_if_data_available<T>(
    scopes: &[PathBuf],
    publish: impl FnOnce() -> T,
) -> Option<T> {
    let state = coordinator().state.try_lock()?;
    if state
        .active
        .iter()
        .any(|lease| lease.exclusive && overlapping(scopes, &lease.paths))
        || state
            .waiting_restores
            .iter()
            .any(|lease| overlapping(scopes, &lease.paths))
    {
        return None;
    }
    Some(publish())
}

/// Rejects ordinary writes while an uncommitted backup restore journal exists in
/// any readable ancestor of `paths`.
///
/// Ancestor directories above the application data root (for example `/root` or
/// restricted container parents) may return `PermissionDenied` or `NotADirectory`
/// when probed; those inaccessible ancestors are skipped rather than blocking all
/// ordinary store writes.
pub(crate) fn ensure_writes_available(paths: &[PathBuf]) -> std::io::Result<()> {
    let mut inspected = std::collections::HashSet::new();
    for path in paths {
        for root in path.ancestors() {
            if !inspected.insert(root) {
                continue;
            }
            match inspect_pending_restore_journal(root) {
                Ok(true) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        zenclash_i18n::text("backup.errors.recovery_required"),
                    ));
                }
                Ok(false) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotADirectory | std::io::ErrorKind::PermissionDenied
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn inspect_pending_restore_journal(root: &Path) -> std::io::Result<bool> {
    use std::io::Read as _;

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct JournalHeader {
        version: u32,
        rollback_name: String,
        staging_name: String,
        #[allow(dead_code)]
        original: [bool; 4],
        #[allow(dead_code)]
        states: [serde_json::Value; 4],
        #[allow(dead_code)]
        remove_empty_data_root: bool,
        committed: bool,
    }

    const JOURNAL_FILE: &str = ".backup-restore.json";
    const MAX_JOURNAL_BYTES: usize = 16 * 1024;

    let path = root.join(JOURNAL_FILE);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.len() > MAX_JOURNAL_BYTES as u64 {
        return Err(std::io::Error::other("invalid backup restore journal file"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_JOURNAL_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(std::io::Error::other(
            "backup restore journal exceeds its size limit",
        ));
    }
    let journal: JournalHeader = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
    if journal.version != 1
        || !valid_generated_dir_name(&journal.rollback_name, ".zenclash-backup-rollback-")
        || !valid_generated_dir_name(&journal.staging_name, ".zenclash-backup-staging-")
    {
        return Err(std::io::Error::other(
            "invalid backup restore journal paths or version",
        ));
    }
    Ok(!journal.committed)
}

fn valid_generated_dir_name(name: &str, prefix: &str) -> bool {
    let Some((pid, sequence)) = name
        .strip_prefix(prefix)
        .and_then(|suffix| suffix.split_once('-'))
    else {
        return false;
    };
    pid.parse::<u32>().is_ok() && sequence.parse::<u64>().is_ok()
}

fn coordinator() -> &'static Coordinator {
    static COORDINATOR: OnceLock<Coordinator> = OnceLock::new();
    COORDINATOR.get_or_init(Coordinator::default)
}

fn acquire(paths: impl IntoIterator<Item = PathBuf>, exclusive: bool) -> DataWriteLease {
    let requested = paths.into_iter().collect::<Vec<_>>();
    let coordinator = coordinator();
    let mut observed_epoch = coordinator.exclusive_releases.load(Ordering::Acquire);
    let mut paths = write_paths(&requested);
    let mut state = coordinator.state.lock();
    loop {
        let current_epoch = coordinator.exclusive_releases.load(Ordering::Acquire);
        if !state.exclusive_release_overlapped(observed_epoch, current_epoch, &paths) {
            observed_epoch = current_epoch;
            break;
        }
        observed_epoch = current_epoch;
        drop(state);
        paths = write_paths(&requested);
        state = coordinator.state.lock();
    }
    state.sequence += 1;
    let id = state.sequence;
    if exclusive {
        state.waiting_restores.push(LeaseRecord {
            id,
            paths: paths.clone(),
            exclusive,
        });
    }
    let mut recheck_before_admit = false;
    loop {
        let current_epoch = coordinator.exclusive_releases.load(Ordering::Acquire);
        let overlapped_release =
            state.exclusive_release_overlapped(observed_epoch, current_epoch, &paths);
        observed_epoch = current_epoch;
        let needs_resolve =
            if overlapped_release {
                true
            } else {
                let active_conflict = state.active.iter().any(|lease| {
                    (exclusive || lease.exclusive) && overlapping(&paths, &lease.paths)
                });
                let restore_precedes = state.waiting_restores.iter().any(|lease| {
                    (!exclusive || lease.id < id) && overlapping(&paths, &lease.paths)
                });
                if !active_conflict && !restore_precedes {
                    if !recheck_before_admit {
                        break;
                    }
                    true
                } else {
                    false
                }
            };
        if needs_resolve {
            // Release the global coordinator lock before performing filesystem
            // canonicalize/read_link syscalls. A preceding restore may have replaced
            // a symlinked directory before or while this operation waited; re-resolve
            // its physical destination and recheck against the release history before
            // admitting the lease.
            drop(state);
            paths = write_paths(&requested);
            state = coordinator.state.lock();
            recheck_before_admit = false;
            if let Some(waiting) = state
                .waiting_restores
                .iter_mut()
                .find(|lease| lease.id == id)
                && waiting.paths != paths
            {
                waiting.paths.clone_from(&paths);
                coordinator.changed.notify_all();
            }
            continue;
        }
        coordinator.changed.wait(&mut state);
        recheck_before_admit = true;
    }
    state.waiting_restores.retain(|lease| lease.id != id);
    state.active.push(LeaseRecord {
        id,
        paths: paths.clone(),
        exclusive,
    });
    DataWriteLease {
        inner: Arc::new(LeaseInner {
            id,
            paths,
            exclusive,
        }),
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

pub(crate) fn coordinated_paths(path: &Path) -> [PathBuf; 2] {
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
    #[test]
    fn memory_publication_holds_the_original_coordinator_guard_and_releases_without_relocking() {
        let root = test_root("memory-publication-guard");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(guard_held) = publish_if_data_available(std::slice::from_ref(&root), || {
                coordinator().state.try_lock().is_none()
            }) {
                assert!(guard_held);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "coordinator never became available"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn memory_publication_rejects_an_active_restore_or_busy_coordinator_without_waiting() {
        let root = test_root("memory-publication-busy");
        let restoring = DataWriteLease::exclusive([root.clone()]);
        assert!(
            publish_if_data_available(std::slice::from_ref(&root), || panic!(
                "active restore publication ran"
            ))
            .is_none()
        );
        drop(restoring);
        let _state = coordinator().state.lock();
        assert!(publish_if_data_available(&[root], || panic!("busy publication ran")).is_none());
    }

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
            let _lease = access.acquire().unwrap();
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
            let _lease = access.acquire().unwrap();
            admitted_tx.send(()).unwrap();
        });
        let mut early = admitted_rx.recv_timeout(Duration::from_millis(100)).ok();
        for _ in 0..1000 {
            std::fs::rename(destination.join("files"), root.join("parked")).unwrap();
            drop(DataWriteLease::shared([root.join("unrelated")]).unwrap());
            std::fs::rename(root.join("parked"), destination.join("files")).unwrap();
            drop(DataWriteLease::shared([root.join("unrelated")]).unwrap());
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
            let _lease = access.acquire().unwrap();
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

    #[cfg(unix)]
    #[test]
    fn a_waiting_writer_cannot_escape_symlink_replacement_under_concurrent_notifications() {
        use std::os::unix::fs::symlink;

        let root = test_root("waiting-symlink-notify-race");
        let data = root.join("data");
        let first = root.join("first");
        let second = root.join("second");
        let unrelated_shared = root.join("unrelated-shared");
        let unrelated_exclusive = root.join("unrelated-exclusive");
        for directory in [
            &data,
            &first,
            &second,
            &unrelated_shared,
            &unrelated_exclusive,
        ] {
            std::fs::create_dir_all(directory).unwrap();
        }
        let logical = data.join("store");
        symlink(&first, &logical).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let shared_stop = Arc::clone(&stop);
        let shared_noise = thread::spawn(move || {
            while !shared_stop.load(Ordering::Relaxed) {
                drop(DataWriteLease::shared([unrelated_shared.clone()]).unwrap());
            }
        });
        let exclusive_stop = Arc::clone(&stop);
        let exclusive_noise = thread::spawn(move || {
            while !exclusive_stop.load(Ordering::Relaxed) {
                drop(DataWriteLease::exclusive([unrelated_exclusive.clone()]));
            }
        });

        let mut escaped = false;
        for _ in 0..80 {
            let _ = std::fs::remove_file(&logical);
            symlink(&first, &logical).unwrap();
            let access = DataWriteAccess::new(&logical);
            let restoring_data = DataWriteLease::exclusive([data.clone()]);
            let restoring_destination = DataWriteLease::exclusive([second.clone()]);
            let (started_tx, started_rx) = mpsc::channel();
            let (admitted_tx, admitted_rx) = mpsc::channel();
            let writer = thread::spawn(move || {
                started_tx.send(()).unwrap();
                let _lease = access.acquire().unwrap();
                admitted_tx.send(()).unwrap();
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            thread::sleep(Duration::from_millis(2));
            std::fs::remove_file(&logical).unwrap();
            symlink(&second, &logical).unwrap();
            drop(restoring_data);
            let after_replace = admitted_rx.recv_timeout(Duration::from_millis(10));
            drop(restoring_destination);
            if after_replace.is_ok() {
                escaped = true;
            } else {
                admitted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            writer.join().unwrap();
            if escaped {
                break;
            }
        }

        stop.store(true, Ordering::Relaxed);
        shared_noise.join().unwrap();
        exclusive_noise.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            !escaped,
            "writer was admitted with stale physical scope under concurrent coordinator notifications"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_waiting_restore_updates_its_physical_scope_and_unblocks_the_old_target() {
        use std::os::unix::fs::symlink;

        let root = test_root("waiting-restore-symlink");
        let data = root.join("data");
        let first = root.join("first");
        let second = root.join("second");
        for directory in [&data, &first, &second] {
            std::fs::create_dir_all(directory).unwrap();
        }
        let logical = data.join("store");
        symlink(&first, &logical).unwrap();

        let restoring_data = DataWriteLease::exclusive([data]);
        let restoring_destination = DataWriteLease::exclusive([second.clone()]);
        let (admitted_tx, admitted_rx) = mpsc::channel();
        let waiting_logical = logical.clone();
        let waiting_restore = thread::spawn(move || {
            let _lease = DataWriteLease::exclusive([waiting_logical]);
            admitted_tx.send(()).unwrap();
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !has_waiting_restore(&first) {
            assert!(
                std::time::Instant::now() < deadline,
                "waiting restore never registered its initial physical target"
            );
            thread::yield_now();
        }

        let (old_target_tx, old_target_rx) = mpsc::channel();
        let old_target_writer = thread::spawn(move || {
            let _lease = DataWriteLease::shared([first]).unwrap();
            old_target_tx.send(()).unwrap();
        });
        assert!(matches!(
            old_target_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        std::fs::remove_file(&logical).unwrap();
        symlink(&second, &logical).unwrap();
        drop(restoring_data);

        // Once the root restore ends and `logical` points to `second`, the waiting
        // restore must wait on `second` and stop blocking writers to `first`.
        old_target_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        old_target_writer.join().unwrap();

        let early_restore = admitted_rx.recv_timeout(Duration::from_millis(100));
        drop(restoring_destination);
        if early_restore.is_err() {
            admitted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        waiting_restore.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            matches!(early_restore, Err(mpsc::RecvTimeoutError::Timeout)),
            "waiting restore used its stale physical scope after the root restore ended"
        );
    }

    #[test]
    fn an_admitted_writer_finishes_after_parent_cancellation_with_a_restore_waiting() {
        let root = test_root("admitted-write");
        std::fs::create_dir_all(&root).unwrap();
        let parent = DataWriteLease::shared([root.clone()]).unwrap();
        let access = DataWriteAccess::new(&root).authorized(&parent);
        let admitted = access.acquire().unwrap();
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
            let _nested = writer_access.acquire().unwrap();
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

    #[cfg(unix)]
    #[test]
    fn ensure_writes_available_ignores_permission_denied_on_ancestor_directories() {
        use std::os::unix::fs::PermissionsExt;

        // Root bypasses directory permission checks.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let root = test_root("ancestor-eperm");
        let child = root.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let target = child.join("store.json");
        let original = std::fs::metadata(&root).unwrap().permissions();
        // Remove read/search on `root` temporarily? Keeping execute (`0o100`) allows
        // traversing to `child` while denying directory listing; removing write/read (`0o000`)
        // triggers `PermissionDenied` when probing `root/.backup-restore.json`.
        let mut restricted = original.clone();
        restricted.set_mode(0o000);
        std::fs::set_permissions(&root, restricted).unwrap();

        let result = ensure_writes_available(std::slice::from_ref(&target));
        std::fs::set_permissions(&root, original).unwrap();
        std::fs::remove_dir_all(&root).unwrap();

        result.expect("PermissionDenied on an ancestor must not block writes");
    }

    fn test_root(label: &str) -> PathBuf {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "zenclash-{label}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }
}
