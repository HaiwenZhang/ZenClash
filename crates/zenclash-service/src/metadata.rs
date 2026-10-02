use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::PROTOCOL_VERSION;

/// Installation record format, versioned separately from the IPC protocol.
pub const METADATA_SCHEMA_VERSION: u32 = 1;
const MAX_METADATA_BYTES: usize = 64 * 1024;
const MAX_AUTHORIZED_USERS: usize = 64;

/// Administrator-approved service installation facts.
///
/// Platform code must validate that the containing directory is protected
/// before reading or writing this record. It contains no session credentials.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InstalledMetadata {
    schema_version: u32,
    protocol_version: u32,
    service_version: String,
    authorized_users: Vec<String>,
    core_sha256: String,
    helper_sha256: String,
}

impl InstalledMetadata {
    /// Builds a current record for one verified installer identity.
    ///
    /// # Errors
    /// Rejects malformed digests or an empty, overlong or invalid user identity.
    pub fn new(
        core_sha256: String,
        helper_sha256: String,
        owner: String,
    ) -> Result<Self, MetadataError> {
        let metadata = Self {
            schema_version: METADATA_SCHEMA_VERSION,
            protocol_version: PROTOCOL_VERSION,
            service_version: env!("CARGO_PKG_VERSION").to_owned(),
            authorized_users: vec![owner],
            core_sha256,
            helper_sha256,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    /// Installation record schema version.
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }
    /// Installed service wire protocol version.
    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }
    /// Installed service build version.
    pub fn service_version(&self) -> &str {
        &self.service_version
    }
    /// OS-verified identities approved during installation.
    pub fn authorized_users(&self) -> &[String] {
        &self.authorized_users
    }
    /// SHA-256 of the administrator-approved kernel copy.
    pub fn core_sha256(&self) -> &str {
        &self.core_sha256
    }

    /// SHA-256 of the administrator-approved service helper copy.
    pub fn helper_sha256(&self) -> &str {
        &self.helper_sha256
    }

    /// Adds an OS-verified identity after separate administrator authorization.
    ///
    /// # Errors
    /// Rejects an invalid identity or a new identity beyond the user limit.
    pub fn authorize_user(&mut self, owner: String) -> Result<(), MetadataError> {
        if !valid_identity(&owner) {
            return Err(MetadataError::Invalid);
        }
        if !self.authorized_users.contains(&owner) {
            if self.authorized_users.len() >= MAX_AUTHORIZED_USERS {
                return Err(MetadataError::TooLarge);
            }
            self.authorized_users.push(owner);
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), MetadataError> {
        if self.schema_version != METADATA_SCHEMA_VERSION {
            return Err(MetadataError::UnsupportedSchema(self.schema_version));
        }
        if self.protocol_version == 0
            || self.service_version.is_empty()
            || self.service_version.len() > 128
            || self.service_version.chars().any(char::is_control)
            || self.authorized_users.is_empty()
            || self.authorized_users.len() > MAX_AUTHORIZED_USERS
            || self
                .authorized_users
                .iter()
                .any(|owner| !valid_identity(owner))
            || self.core_sha256.len() != 64
            || !self
                .core_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.helper_sha256.len() != 64
            || !self
                .helper_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(MetadataError::Invalid);
        }
        Ok(())
    }
}

fn valid_identity(owner: &str) -> bool {
    !owner.is_empty() && owner.len() <= 256 && !owner.chars().any(char::is_control)
}

/// Failure to load or atomically commit installation metadata.
#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    /// Unknown record formats must not be overwritten.
    #[error("unsupported service metadata schema {0}")]
    UnsupportedSchema(u32),
    /// The record exceeded the bounded storage budget.
    #[error("service metadata exceeds its storage budget")]
    TooLarge,
    /// A record field violates the installation schema.
    #[error("invalid service installation metadata")]
    Invalid,
    /// A pre-release record cannot authenticate the installed helper bytes.
    #[error(
        "service installation metadata lacks an approved helper digest; administrator recovery is required"
    )]
    Incomplete,
    /// JSON could not be read or serialized.
    #[error("invalid service metadata JSON")]
    Json(#[source] serde_json::Error),
    /// The protected storage operation failed.
    #[error("service metadata storage failed")]
    Io(#[from] std::io::Error),
    /// A secure temporary filename could not be generated.
    #[error("system random source unavailable for metadata commit")]
    Random,
}

/// Reads and validates a bounded installation record.
///
/// A future protocol may be reported by a known schema; compatibility belongs
/// to the handshake and must not be conflated with the storage format.
///
/// # Errors
/// Reports storage errors, over-budget or malformed JSON, unsupported schemas,
/// missing approved helper digests and invalid record fields.
pub fn read_metadata(path: &Path) -> Result<InstalledMetadata, MetadataError> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((MAX_METADATA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(MetadataError::TooLarge);
    }
    let version: serde_json::Value = serde_json::from_slice(&bytes).map_err(MetadataError::Json)?;
    let schema = version
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or(MetadataError::Invalid)?;
    if schema != u64::from(METADATA_SCHEMA_VERSION) {
        return Err(MetadataError::UnsupportedSchema(
            u32::try_from(schema).unwrap_or(u32::MAX),
        ));
    }
    if version.get("helper_sha256").is_none() {
        return Err(MetadataError::Incomplete);
    }
    let metadata: InstalledMetadata =
        serde_json::from_slice(&bytes).map_err(MetadataError::Json)?;
    metadata.validate()?;
    Ok(metadata)
}

/// Commits metadata using a same-directory temporary file and atomic replace.
///
/// Existing unknown, malformed, or unreadable records are preserved. All
/// validation and serialization happen before the existing file is changed.
/// Call only from the installer or a background task with a protected parent.
///
/// # Errors
/// Rejects invalid or over-budget metadata and unreadable or unrecognized
/// existing records. Reports random-source, write, sync and atomic-replace
/// failures without accepting an incomplete new record.
pub fn write_metadata_atomic(
    path: &Path,
    metadata: &InstalledMetadata,
) -> Result<(), MetadataError> {
    metadata.validate()?;
    let bytes = serde_json::to_vec(metadata).map_err(MetadataError::Json)?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(MetadataError::TooLarge);
    }
    match read_metadata(path) {
        Ok(_) => {}
        Err(MetadataError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = path.parent().ok_or(MetadataError::Invalid)?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| MetadataError::Random)?;
    let name = nonce
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let temporary = parent.join(format!(".zenclash-metadata-{name}.tmp"));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(MetadataError::Io)
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    fs::rename(temporary, path)?;
    File::open(path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "metadata has no parent")
    })?)?
    .sync_all()
}

#[cfg(windows)]
fn replace_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: Both UTF-16 paths are owned and NUL-terminated for this call.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn protocol_one_record_remains_readable_for_repair_but_cannot_handshake() {
        let directory = directory();
        let path = directory.join("installation.json");
        let mut metadata =
            InstalledMetadata::new("a".repeat(64), "c".repeat(64), "1000".into()).unwrap();
        metadata.protocol_version = 1;
        write_metadata_atomic(&path, &metadata).unwrap();
        let before = fs::read(&path).unwrap();
        let installed = read_metadata(&path).unwrap();
        assert_eq!(installed.schema_version(), METADATA_SCHEMA_VERSION);
        assert_eq!(installed.protocol_version(), 1);
        assert!(
            !crate::ProtocolInfo {
                protocol_version: installed.protocol_version(),
                service_version: installed.service_version().to_owned(),
            }
            .is_compatible()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::remove_dir_all(directory).unwrap();
    }

    use super::*;

    fn directory() -> std::path::PathBuf {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let name = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let directory = std::env::temp_dir().join(format!("zenclash-metadata-{name}"));
        fs::create_dir(&directory).unwrap();
        directory
    }

    #[test]
    fn metadata_updates_replace_the_record_and_leave_no_staging_file() {
        let directory = directory();
        let path = directory.join("installation.json");
        let first = InstalledMetadata::new("a".repeat(64), "c".repeat(64), "1000".into()).unwrap();
        write_metadata_atomic(&path, &first).unwrap();
        let mut second =
            InstalledMetadata::new("b".repeat(64), "d".repeat(64), "1000".into()).unwrap();
        second.authorize_user("1001".into()).unwrap();
        write_metadata_atomic(&path, &second).unwrap();
        assert_eq!(read_metadata(&path).unwrap(), second);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_candidate_and_unknown_previous_schema_preserve_valid_bytes() {
        let directory = directory();
        let path = directory.join("installation.json");
        let valid = InstalledMetadata::new("a".repeat(64), "c".repeat(64), "1000".into()).unwrap();
        write_metadata_atomic(&path, &valid).unwrap();
        let original = fs::read(&path).unwrap();
        let mut invalid = valid.clone();
        invalid.core_sha256.clear();
        assert!(matches!(
            write_metadata_atomic(&path, &invalid),
            Err(MetadataError::Invalid)
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::write(&path, br#"{"schema_version":999}"#).unwrap();
        assert!(matches!(
            write_metadata_atomic(&path, &valid),
            Err(MetadataError::UnsupportedSchema(999))
        ));
        assert_eq!(fs::read(&path).unwrap(), br#"{"schema_version":999}"#);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn known_schema_reports_future_protocol_without_mistaking_it_for_schema() {
        let directory = directory();
        let path = directory.join("installation.json");
        let mut metadata =
            InstalledMetadata::new("a".repeat(64), "c".repeat(64), "1000".into()).unwrap();
        metadata.protocol_version = PROTOCOL_VERSION + 1;
        write_metadata_atomic(&path, &metadata).unwrap();
        assert_eq!(
            read_metadata(&path).unwrap().protocol_version(),
            PROTOCOL_VERSION + 1
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn records_without_approved_helper_digest_are_preserved_and_require_recovery() {
        let directory = directory();
        let path = directory.join("installation.json");
        let metadata =
            InstalledMetadata::new("a".repeat(64), "c".repeat(64), "1000".into()).unwrap();
        let mut value = serde_json::to_value(&metadata).unwrap();
        value.as_object_mut().unwrap().remove("helper_sha256");
        let legacy = serde_json::to_vec(&value).unwrap();
        fs::write(&path, &legacy).unwrap();
        assert!(matches!(
            read_metadata(&path),
            Err(MetadataError::Incomplete)
        ));
        assert!(matches!(
            write_metadata_atomic(&path, &metadata),
            Err(MetadataError::Incomplete)
        ));
        assert_eq!(fs::read(path).unwrap(), legacy);
        fs::remove_dir_all(directory).unwrap();
    }
}
