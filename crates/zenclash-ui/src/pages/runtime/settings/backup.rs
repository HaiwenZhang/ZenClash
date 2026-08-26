use std::path::PathBuf;

use zenclash_core::{
    AppPreferences, ControlledConfigStore, ProfileCatalog, ProfileStore, YamlOverrideCatalog,
    YamlOverrideStore,
};

use super::super::RuntimeData;

mod actions;
mod view;
mod workflow;

pub(super) use workflow::restore_prepared;

pub(super) struct RestoreOutcome {
    preferences: AppPreferences,
    catalog: ProfileCatalog,
    profile_store: ProfileStore,
    controlled_store: ControlledConfigStore,
    controlled_config: serde_json::Value,
    override_store: YamlOverrideStore,
    override_catalog: YamlOverrideCatalog,
    profile_path: PathBuf,
    page_data: RuntimeData,
    file_count: usize,
    payload_bytes: u64,
    cleanup_warning: Option<String>,
}

pub(super) fn format_backup_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format_backup_unit(bytes, 1024 * 1024, "MiB")
    } else if bytes >= 1024 {
        format_backup_unit(bytes, 1024, "KiB")
    } else {
        format!("{bytes} B")
    }
}

fn format_backup_unit(bytes: u64, unit: u64, suffix: &str) -> String {
    let whole = bytes / unit;
    let decimal = bytes % unit * 10 / unit;
    format!("{whole}.{decimal} {suffix}")
}

#[cfg(test)]
mod tests {
    use super::format_backup_size;

    #[test]
    fn backup_size_format_uses_integer_tenths_without_precision_loss() {
        assert_eq!(format_backup_size(512), "512 B");
        assert_eq!(format_backup_size(1_536), "1.5 KiB");
        assert_eq!(format_backup_size(2_621_440), "2.5 MiB");
    }
}
