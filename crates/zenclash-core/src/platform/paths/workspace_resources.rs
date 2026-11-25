//! Development workspace layout; these paths do not locate or install executables.

use std::path::{Path, PathBuf};

use super::ApplicationPlatform;
use crate::CoreKind;

/// Immutable resource and development-home locations beneath an explicit workspace.
#[derive(Clone, Debug)]
pub struct WorkspaceResources {
    root: PathBuf,
}

impl WorkspaceResources {
    /// Uses the caller's workspace without reading environment variables or creating files.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Returns the caller-provided workspace root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the fallback source profile in the development/package input tree.
    #[must_use]
    pub fn default_profile(&self) -> PathBuf {
        self.root.join("platforms/common/default.yaml")
    }

    /// Returns the immutable direct-only recovery profile in the workspace.
    #[must_use]
    pub fn recovery_profile(&self) -> PathBuf {
        self.root.join("platforms/common/recovery.yaml")
    }

    /// Returns the workspace executable candidate; validation belongs to the caller.
    #[must_use]
    pub fn core_binary(&self, kind: CoreKind) -> PathBuf {
        self.root.join("bin").join(core_executable_filename(kind))
    }

    /// Returns the established development working directory for one core.
    #[must_use]
    pub fn core_home(&self, kind: CoreKind) -> PathBuf {
        self.root
            .join(format!("target/zenclash-{}", kind.executable_stem()))
    }
}

pub(crate) fn core_executable_filename(kind: CoreKind) -> String {
    executable_filename_for_platform(kind, ApplicationPlatform::current())
}

fn executable_filename_for_platform(kind: CoreKind, platform: ApplicationPlatform) -> String {
    if matches!(platform, ApplicationPlatform::Windows) {
        format!("{}.exe", kind.executable_stem())
    } else {
        kind.executable_stem().to_owned()
    }
}

#[cfg(test)]
mod tests;
