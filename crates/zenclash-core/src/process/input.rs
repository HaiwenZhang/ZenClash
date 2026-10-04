//! File-backed stdin avoids a blocking writer and is removed with its handles.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    path::Path,
};

use crate::{MihomoError, MihomoResult};

pub(super) fn read_config(path: &Path) -> MihomoResult<String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Opening a substituted FIFO must not wait for a writer.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|error| MihomoError::InvalidInput(error.to_string()))?;
    let regular = file
        .metadata()
        .map_err(|error| MihomoError::InvalidInput(error.to_string()))?
        .is_file();
    #[cfg(windows)]
    let regular = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, GetFileType};
        // SAFETY: file owns the live handle throughout this synchronous query.
        regular && unsafe { GetFileType(file.as_raw_handle().cast()) } == FILE_TYPE_DISK
    };
    if !regular {
        return Err(MihomoError::InvalidInput(zenclash_i18n::text(
            "core_page.service.input_not_regular",
        )));
    }
    let mut bytes = Vec::new();
    file.take(crate::profiles::MAX_PROFILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MihomoError::InvalidInput(error.to_string()))?;
    if bytes.len() > crate::profiles::MAX_PROFILE_BYTES {
        return Err(MihomoError::InvalidInput(zenclash_i18n::text_with(
            "core_page.service.payload_too_large",
            &[(
                "limit",
                (crate::profiles::MAX_PROFILE_BYTES / (1024 * 1024)).to_string(),
            )],
        )));
    }
    String::from_utf8(bytes).map_err(|error| MihomoError::InvalidInput(error.to_string()))
}

pub(super) fn snapshot(home: &Path, payload: &str) -> MihomoResult<File> {
    prepare(home, payload).map_err(|error| MihomoError::Process(error.to_string()))
}

fn prepare(home: &Path, payload: &str) -> std::io::Result<File> {
    std::fs::create_dir_all(home)?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|error| std::io::Error::other(error.to_string()))?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let nonce: String = nonce
        .into_iter()
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 15)]),
            ]
        })
        .collect();
    let path = home.join(format!(".zenclash-start-{nonce}.yaml"));
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::{
            Foundation::{GENERIC_READ, GENERIC_WRITE},
            Storage::FileSystem::{
                DELETE, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE, FILE_SHARE_READ,
            },
        };
        options
            .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
    }
    let mut file = options.open(&path)?;
    #[cfg(unix)]
    if let Err(error) = std::fs::remove_file(&path) {
        drop(file);
        return Err(error);
    }
    file.write_all(payload.as_bytes())?;
    file.rewind()?;
    Ok(file)
}
