//! Profile and writable-home policy shared by local launch and resource-only preparation.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use super::{AppDataPaths, BundledResources, WorkspaceResources, managed_core_data_directory};
use crate::CoreKind;

pub(crate) struct CoreRuntimePaths {
    pub(crate) config_file: PathBuf,
    pub(crate) home_dir: PathBuf,
}

impl CoreRuntimePaths {
    pub(crate) fn discover(
        workspace: &Path,
        kind: CoreKind,
        config_override: Option<&Path>,
    ) -> Self {
        let resources = BundledResources::discover().ok();
        Self::resolve(
            &WorkspaceResources::new(workspace),
            kind,
            config_override,
            resources.as_ref(),
            managed_core_data_directory().as_ref(),
            |name| std::env::var_os(name),
        )
    }

    pub(crate) fn discover_home(workspace: &Path, kind: CoreKind) -> PathBuf {
        let resources = BundledResources::discover().ok();
        resolve_home(
            &WorkspaceResources::new(workspace),
            kind,
            resources.as_ref(),
            managed_core_data_directory().as_ref(),
            |name| std::env::var_os(name),
        )
    }

    fn resolve(
        workspace: &WorkspaceResources,
        kind: CoreKind,
        config_override: Option<&Path>,
        resources: Option<&BundledResources>,
        managed_data: Option<&AppDataPaths>,
        mut environment: impl FnMut(&str) -> Option<OsString>,
    ) -> Self {
        let config_file = config_override
            .map(Path::to_path_buf)
            .or_else(|| environment("ZENCLASH_CONFIG").map(PathBuf::from))
            .or_else(|| resources.and_then(|resources| resources.resource_file("profile.yaml")))
            .unwrap_or_else(|| workspace.default_profile());
        let home_dir = resolve_home(workspace, kind, resources, managed_data, environment);
        Self {
            config_file,
            home_dir,
        }
    }
}

fn resolve_home(
    workspace: &WorkspaceResources,
    kind: CoreKind,
    resources: Option<&BundledResources>,
    managed_data: Option<&AppDataPaths>,
    mut environment: impl FnMut(&str) -> Option<OsString>,
) -> PathBuf {
    environment("ZENCLASH_CORE_HOME")
        .or_else(|| environment(kind.home_environment_variable()))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            resources
                .filter(|resources| resources.directory().is_some())
                .and(managed_data)
                .map_or_else(|| workspace.core_home(kind), |data| data.core_home(kind))
        })
}

#[cfg(test)]
mod tests;
