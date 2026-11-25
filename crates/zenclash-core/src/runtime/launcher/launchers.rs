//! Concrete local and privileged-service [`CoreProcessLauncher`] adapters.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use super::{
    CoreLaunchDescriptor, CoreProcessLauncher, CoreProcessLease, LauncherFuture,
    LeftoverLeaseTracker, leftover::ensure_reclaimed_before_start_blocking,
};
use crate::mihomo::errors::{MihomoError, MihomoResult};

type LaunchCallback = Arc<
    dyn Fn(CoreLaunchDescriptor) -> LauncherFuture<'static, MihomoResult<Arc<dyn CoreProcessLease>>>
        + Send
        + Sync,
>;

fn blocking_spawn_callback<F>(spawn: F) -> LaunchCallback
where
    F: Fn(CoreLaunchDescriptor) -> MihomoResult<Arc<dyn CoreProcessLease>> + Send + Sync + 'static,
{
    let spawn = Arc::new(spawn);
    Arc::new(move |descriptor| {
        let spawn = Arc::clone(&spawn);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || spawn(descriptor))
                .await
                .map_err(|error| {
                    MihomoError::Process(format!("core process launch task failed: {error}"))
                })?
        })
    })
}

/// Concrete zero-privilege local [`CoreProcessLauncher`] adapter gated by a
/// [`LeftoverLeaseTracker`].
///
/// Before spawning a replacement child process, `LocalLauncher` reclaims any
/// pending leftover leases on a blocking worker thread so two local cores can
/// never coexist.
#[derive(Clone)]
pub struct LocalLauncher {
    leftovers: LeftoverLeaseTracker,
    spawn: LaunchCallback,
}

impl std::fmt::Debug for LocalLauncher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalLauncher")
            .field("backend", &self.backend_name())
            .field("pending_leftovers", &self.leftovers.pending_count())
            .finish()
    }
}

impl LocalLauncher {
    /// Creates a `LocalLauncher` backed by an asynchronous launch callback.
    #[must_use]
    pub fn new<F>(leftovers: LeftoverLeaseTracker, spawn: F) -> Self
    where
        F: Fn(
                CoreLaunchDescriptor,
            ) -> LauncherFuture<'static, MihomoResult<Arc<dyn CoreProcessLease>>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            leftovers,
            spawn: Arc::new(spawn),
        }
    }

    /// Creates a `LocalLauncher` backed by a blocking process-spawn callback executed
    /// inside [`tokio::task::spawn_blocking`].
    #[must_use]
    pub fn with_blocking<F>(leftovers: LeftoverLeaseTracker, spawn: F) -> Self
    where
        F: Fn(CoreLaunchDescriptor) -> MihomoResult<Arc<dyn CoreProcessLease>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            leftovers,
            spawn: blocking_spawn_callback(spawn),
        }
    }

    /// Returns the leftover-lease tracker guarding this launcher.
    #[must_use]
    pub const fn leftovers(&self) -> &LeftoverLeaseTracker {
        &self.leftovers
    }
}

impl CoreProcessLauncher for LocalLauncher {
    fn backend_name(&self) -> &'static str {
        "local"
    }

    fn launch<'a>(
        &'a self,
        descriptor: &'a CoreLaunchDescriptor,
    ) -> LauncherFuture<'a, MihomoResult<Arc<dyn CoreProcessLease>>> {
        let descriptor = descriptor.clone();
        let leftovers = self.leftovers.clone();
        let spawn = Arc::clone(&self.spawn);
        Box::pin(async move {
            ensure_reclaimed_before_start_blocking(&leftovers).await?;
            spawn(descriptor).await
        })
    }
}

/// Concrete thin privileged-service [`CoreProcessLauncher`] adapter gated by a
/// [`LeftoverLeaseTracker`].
///
/// Encapsulates the user-readable application home and optional trusted core path
/// shared with the privileged helper, ensuring leftover reclamation precedes every
/// launch request.
#[derive(Clone)]
pub struct ServiceLauncher {
    source_home: PathBuf,
    core_source: Option<PathBuf>,
    leftovers: LeftoverLeaseTracker,
    spawn: LaunchCallback,
}

impl std::fmt::Debug for ServiceLauncher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceLauncher")
            .field("backend", &self.backend_name())
            .field("source_home", &self.source_home)
            .field("core_source", &self.core_source)
            .field("pending_leftovers", &self.leftovers.pending_count())
            .finish()
    }
}

impl ServiceLauncher {
    /// Creates a `ServiceLauncher` backed by an asynchronous helper launch callback.
    #[must_use]
    pub fn new<F>(
        source_home: PathBuf,
        core_source: Option<PathBuf>,
        leftovers: LeftoverLeaseTracker,
        spawn: F,
    ) -> Self
    where
        F: Fn(
                CoreLaunchDescriptor,
            ) -> LauncherFuture<'static, MihomoResult<Arc<dyn CoreProcessLease>>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            source_home,
            core_source,
            leftovers,
            spawn: Arc::new(spawn),
        }
    }

    /// Creates a `ServiceLauncher` backed by a blocking helper launch callback executed
    /// inside [`tokio::task::spawn_blocking`].
    #[must_use]
    pub fn with_blocking<F>(
        source_home: PathBuf,
        core_source: Option<PathBuf>,
        leftovers: LeftoverLeaseTracker,
        spawn: F,
    ) -> Self
    where
        F: Fn(CoreLaunchDescriptor) -> MihomoResult<Arc<dyn CoreProcessLease>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            source_home,
            core_source,
            leftovers,
            spawn: blocking_spawn_callback(spawn),
        }
    }

    /// Returns the user-readable application home directory bound to this service launcher.
    #[must_use]
    pub fn source_home(&self) -> &Path {
        &self.source_home
    }

    /// Returns the optional selected core executable path for trusted-path verification.
    #[must_use]
    pub fn core_source(&self) -> Option<&Path> {
        self.core_source.as_deref()
    }

    /// Returns the leftover-lease tracker guarding this launcher.
    #[must_use]
    pub const fn leftovers(&self) -> &LeftoverLeaseTracker {
        &self.leftovers
    }
}

impl CoreProcessLauncher for ServiceLauncher {
    fn backend_name(&self) -> &'static str {
        "service"
    }

    fn launch<'a>(
        &'a self,
        descriptor: &'a CoreLaunchDescriptor,
    ) -> LauncherFuture<'a, MihomoResult<Arc<dyn CoreProcessLease>>> {
        let descriptor = descriptor.clone();
        let leftovers = self.leftovers.clone();
        let spawn = Arc::clone(&self.spawn);
        Box::pin(async move {
            ensure_reclaimed_before_start_blocking(&leftovers).await?;
            spawn(descriptor).await
        })
    }
}
