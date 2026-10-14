use crate::application_commands::BackupRestoreReceipt;

use super::super::PageSnapshotData;

mod actions;
mod view;
mod workflow;

pub(in crate::pages::runtime) struct RestoreOutcome {
    pub(in crate::pages::runtime) restored: BackupRestoreReceipt,
    pub(in crate::pages::runtime) page_data: PageSnapshotData,
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

#[cfg(test)]
mod ui_tests;
