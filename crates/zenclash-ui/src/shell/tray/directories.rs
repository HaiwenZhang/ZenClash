//! Tray directory entries derived from application locations and actual runtime facts.

use std::path::{Path, PathBuf};

pub(in crate::shell) fn load_profile_catalog_and_directories(
    profile_store: Option<&zenclash_core::ProfileStore>,
    data_root: Option<&Path>,
    profile_path: Option<&Path>,
    runtime: &zenclash_core::CoreRuntimeDescriptor,
) -> (
    Result<zenclash_core::ProfileCatalog, String>,
    Vec<(String, PathBuf)>,
) {
    let directories = tray_directories(profile_path, runtime, data_root);
    let catalog = match profile_store {
        Some(store) => store.load().map_err(|error| error.to_string()),
        None => Err(zenclash_i18n::text("profiles.errors.store_unavailable")),
    };
    (catalog, directories)
}

pub(in crate::shell) fn tray_directories(
    profile_path: Option<&Path>,
    runtime: &zenclash_core::CoreRuntimeDescriptor,
    data_root: Option<&Path>,
) -> Vec<(String, PathBuf)> {
    let mut directories = Vec::new();
    if let Some(config_dir) = profile_path.and_then(Path::parent) {
        directories.push((
            zenclash_i18n::text("app.directories.config"),
            config_dir.to_path_buf(),
        ));
    }
    if let Some(data) = data_root {
        directories.push((
            zenclash_i18n::text("app.directories.data"),
            data.to_path_buf(),
        ));
    }
    if let Some(home) = runtime.home_dir() {
        directories.push((
            zenclash_i18n::text_with(
                "app.directories.core_working",
                &[("core", runtime.kind().display_name().to_owned())],
            ),
            home.to_path_buf(),
        ));
    }
    if let Some(resources) = installed_resources_dir() {
        directories.push((zenclash_i18n::text("app.directories.resources"), resources));
    }
    directories
}

fn installed_resources_dir() -> Option<PathBuf> {
    zenclash_core::BundledResources::discover()
        .ok()?
        .directory()
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests;
