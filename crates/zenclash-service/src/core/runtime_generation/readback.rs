//! Reads manifest-declared provider caches from the private runtime directory.
//! Undeclared paths are rejected before target lookup.

use super::assets::{
    RUNTIME_CONFIG_FILE_NAME, destination_key, invalid_asset, resolve_in_generation, validate_destination,
};
use super::staging::{MANIFEST_FILE_NAME, modified_nanos, read_manifest};
use crate::RuntimeFileOutcome;
use crate::core::auth::{AuthenticatedOwner, ServiceError};
use crate::core::paths::service_paths;
use std::path::Path;

/// Stays below the transport's 10 MiB in-memory response limit after hex encoding.
const RUNTIME_FILE_CHUNK_BYTES: u64 = 2 * 1024 * 1024;

pub(crate) async fn read_runtime_file(
    owner: &AuthenticatedOwner,
    destination: &str,
    offset: u64,
) -> Result<RuntimeFileOutcome, ServiceError> {
    let generation = service_paths()
        .map_err(|error| invalid_asset(error.to_string()))?
        .for_owner(&owner.identity)
        .runtime_dir();
    let key = destination_key(&validate_destination(destination)?)?;
    let manifest = read_manifest(&generation).await.map_err(invalid_asset)?;
    if !manifest.remote_providers.contains_key(&key) {
        return Err(invalid_asset(format!(
            "runtime file {key:?} is not a declared provider cache"
        )));
    }
    read_chunk(&generation, &resolve_in_generation(&generation, destination)?, offset).await
}

async fn read_chunk(generation: &Path, path: &Path, offset: u64) -> Result<RuntimeFileOutcome, ServiceError> {
    use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

    let unreadable = |error: std::io::Error| invalid_asset(format!("runtime file {path:?} cannot be read: {error}"));
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(RuntimeFileOutcome::Absent),
        Err(error) => return Err(unreadable(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid_asset(format!("runtime file {path:?} is not an ordinary file")));
    }
    // Resolve filesystem aliases (including Windows 8.3 names) before checking reserved files.
    let canonical = tokio::fs::canonicalize(path).await.map_err(unreadable)?;
    let canonical_generation = tokio::fs::canonicalize(generation).await.map_err(unreadable)?;
    if !canonical.starts_with(&canonical_generation)
        || canonical
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.eq_ignore_ascii_case(RUNTIME_CONFIG_FILE_NAME) || name.eq_ignore_ascii_case(MANIFEST_FILE_NAME)
            })
    {
        return Err(invalid_asset(format!(
            "runtime file {path:?} resolves to a file the generation owns"
        )));
    }
    let mut file = tokio::fs::File::open(path).await.map_err(unreadable)?;
    let metadata = file.metadata().await.map_err(unreadable)?;
    let len = metadata.len();
    let mut content = Vec::new();
    if offset < len {
        file.seek(std::io::SeekFrom::Start(offset)).await.map_err(unreadable)?;
        file.take(RUNTIME_FILE_CHUNK_BYTES)
            .read_to_end(&mut content)
            .await
            .map_err(unreadable)?;
    }
    Ok(RuntimeFileOutcome::Chunk {
        hex: hex_encode(&content),
        len,
        mtime_ns: modified_nanos(&metadata).and_then(|nanos| u64::try_from(nanos).ok()),
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}
