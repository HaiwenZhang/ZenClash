use std::path::Path;

use super::{AutostartError, AutostartResult, AutostartStatus};

pub(super) fn default_entry_path() -> AutostartResult<Option<std::path::PathBuf>> {
    Err(AutostartError::Path(zenclash_i18n::text(
        "autostart.details.unsupported_platform",
    )))
}

pub(super) fn status(
    _executable: &Path,
    _entry_path: Option<&Path>,
) -> AutostartResult<AutostartStatus> {
    Err(AutostartError::Path(zenclash_i18n::text(
        "autostart.details.unsupported_platform",
    )))
}

pub(super) fn set_enabled(
    _executable: &Path,
    _entry_path: Option<&Path>,
    _enabled: bool,
) -> AutostartResult<()> {
    Err(AutostartError::Path(zenclash_i18n::text(
        "autostart.details.unsupported_platform",
    )))
}
