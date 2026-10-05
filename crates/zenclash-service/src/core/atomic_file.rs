// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
use std::path::Path;

#[cfg(unix)]
pub(crate) async fn replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    tokio::fs::rename(source, destination).await
}

#[cfg(windows)]
pub(crate) async fn replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW};

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: Both paths are NUL-terminated UTF-16 buffers that remain alive for the call.
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn replace_overwrites_an_existing_destination() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("zenclash-service-atomic-replace-{}", std::process::id()));
        let source = root.join("state.json.tmp");
        let destination = root.join("state.json");
        std::fs::create_dir_all(&root)?;
        std::fs::write(&source, b"new")?;
        std::fs::write(&destination, b"old")?;

        super::replace(&source, &destination).await?;

        assert!(!source.exists());
        assert_eq!(std::fs::read(&destination)?, b"new");
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
