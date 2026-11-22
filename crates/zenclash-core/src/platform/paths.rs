//! Application data and package-layout discovery shared by stores, cores and the shell.

use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

use crate::CoreKind;

mod core_runtime;
mod installation_helper;
mod workspace_resources;
pub(crate) use core_runtime::CoreRuntimePaths;
pub(crate) use installation_helper::service_installation_helper;
pub use workspace_resources::WorkspaceResources;
pub(crate) use workspace_resources::core_executable_filename;

#[derive(Clone, Copy)]
enum ApplicationPlatform {
    Macos,
    Windows,
    Unix,
}

impl ApplicationPlatform {
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

#[derive(Clone, Copy)]
enum HomeCompatibility {
    PersistentStores,
    ManagedCore,
}

/// Persistent application paths beneath one explicit or platform-selected root.
#[derive(Clone, Debug)]
pub struct AppDataPaths {
    root: PathBuf,
}

impl AppDataPaths {
    /// Derives the established application layout beneath an explicit root.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolves the platform default without creating files or directories.
    /// Explicit platform data variables take precedence over home fallback.
    #[must_use]
    pub fn discover() -> Option<Self> {
        resolve_data_root(
            ApplicationPlatform::current(),
            HomeCompatibility::PersistentStores,
            |name| std::env::var_os(name),
        )
        .map(Self::new)
    }

    /// Returns the common persistent data root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Returns the managed subscription directory.
    #[must_use]
    pub fn profiles(&self) -> PathBuf {
        self.root.join("profiles")
    }
    /// Returns the application preferences file.
    #[must_use]
    pub fn preferences(&self) -> PathBuf {
        self.root.join("preferences.json")
    }
    /// Returns the persistent controlled-configuration directory.
    #[must_use]
    pub fn controlled_config(&self) -> PathBuf {
        self.root.join("controlled-config")
    }
    /// Returns the ordered YAML override directory.
    #[must_use]
    pub fn yaml_overrides(&self) -> PathBuf {
        self.root.join("yaml-overrides")
    }
    /// Returns the default working directory for one managed core.
    #[must_use]
    pub fn core_home(&self, kind: CoreKind) -> PathBuf {
        self.root.join(kind.executable_stem())
    }
}

fn resolve_data_root(
    platform: ApplicationPlatform,
    compatibility: HomeCompatibility,
    mut environment: impl FnMut(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    let home_variables: &[&str] = match compatibility {
        HomeCompatibility::PersistentStores => &["HOME", "USERPROFILE"],
        HomeCompatibility::ManagedCore => match platform {
            ApplicationPlatform::Windows => &["USERPROFILE"],
            ApplicationPlatform::Macos | ApplicationPlatform::Unix => &["HOME"],
        },
    };
    match platform {
        ApplicationPlatform::Macos => resolve_home_directory(home_variables, &mut environment)
            .map(|home| home.join("Library/Application Support/ZenClash")),
        ApplicationPlatform::Windows => environment("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                resolve_home_directory(home_variables, &mut environment)
                    .map(|home| home.join("AppData/Local"))
            })
            .map(|data| data.join("ZenClash")),
        ApplicationPlatform::Unix => environment("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                resolve_home_directory(home_variables, &mut environment)
                    .map(|home| home.join(".local/share"))
            })
            .map(|data| data.join("zenclash")),
    }
}

fn resolve_home_directory(
    variables: &[&str],
    environment: &mut impl FnMut(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    variables
        .iter()
        .find_map(|name| environment(name))
        .map(PathBuf::from)
}

pub(crate) fn managed_core_data_directory() -> Option<AppDataPaths> {
    if !cfg!(any(unix, target_os = "windows")) {
        return None;
    }
    resolve_data_root(
        ApplicationPlatform::current(),
        HomeCompatibility::ManagedCore,
        |name| std::env::var_os(name),
    )
    .map(AppDataPaths::new)
}

#[cfg(any(target_os = "linux", test))]
fn resolve_user_configuration_directory(
    mut environment: impl FnMut(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    environment("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            environment("HOME")
                .or_else(|| environment("USERPROFILE"))
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
}

#[cfg(target_os = "linux")]
pub(crate) fn user_configuration_directory() -> Option<PathBuf> {
    resolve_user_configuration_directory(|name| std::env::var_os(name))
}

/// Ordered packaged-resource candidates derived from the application executable.
#[derive(Clone, Debug)]
pub struct BundledResources {
    candidates: Vec<PathBuf>,
}

impl BundledResources {
    /// Discovers resource locations beside the currently running application.
    ///
    /// # Errors
    /// Returns the operating system error when the current executable is unavailable.
    pub fn discover() -> io::Result<Self> {
        Ok(Self::for_executable(
            &std::env::current_exe()?,
            ApplicationPlatform::current(),
        ))
    }

    fn for_executable(executable: &Path, platform: ApplicationPlatform) -> Self {
        let Some(directory) = executable.parent() else {
            return Self {
                candidates: Vec::new(),
            };
        };
        let candidates = match platform {
            ApplicationPlatform::Macos => directory
                .parent()
                .map(|contents| vec![contents.join("Resources")])
                .unwrap_or_default(),
            ApplicationPlatform::Windows => vec![directory.join("resources")],
            ApplicationPlatform::Unix => {
                let mut candidates = vec![directory.join("resources")];
                if let Some(prefix) = directory.parent() {
                    candidates.push(prefix.join("lib/zenclash"));
                    candidates.push(prefix.join("share/zenclash"));
                }
                candidates
            }
        };
        Self { candidates }
    }

    /// Returns the first existing resource directory in package search order.
    #[must_use]
    pub fn directory(&self) -> Option<&Path> {
        self.candidates
            .iter()
            .find(|path| path.is_dir())
            .map(PathBuf::as_path)
    }

    pub(crate) fn resource_file(&self, name: &str) -> Option<PathBuf> {
        self.candidates
            .iter()
            .map(|directory| directory.join(name))
            .find(|path| path.is_file())
    }
}

#[cfg(test)]
mod tests;
