use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use thiserror::Error;

#[cfg(target_os = "linux")]
#[path = "tun_permissions/linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "tun_permissions/macos.rs"]
mod platform;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
#[path = "tun_permissions/unsupported.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "tun_permissions/windows.rs"]
mod platform;

/// Result type used by native TUN permission operations.
pub type TunPermissionResult<T> = Result<T, TunPermissionError>;

/// Failure while inspecting or explicitly granting Mihomo TUN privileges.
#[derive(Debug, Error)]
pub enum TunPermissionError {
    /// The configured path is absent, invalid, or is not a supported Mihomo core.
    #[error("Mihomo 内核路径不可用于 TUN 授权：{0}")]
    InvalidBinary(String),
    /// The operating system does not provide a supported privilege workflow.
    #[error("当前平台不支持自动安装 TUN 权限：{0}")]
    Unsupported(String),
    /// A native permission command or privilege prompt failed.
    #[error("TUN 权限操作失败：{0}")]
    Platform(String),
    /// The native operation returned successfully but the permission readback failed.
    #[error("TUN 权限写入后校验失败：{0}")]
    Verification(String),
}

/// Point-in-time privilege state for the selected Mihomo executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunPermissionStatus {
    /// Whether the current core can create a TUN device without another prompt.
    pub granted: bool,
    /// Whether `ZenClash` can offer a native, user-confirmed authorization action.
    pub can_request: bool,
    /// Whether authorizing requires replacing the current application process.
    pub requires_relaunch: bool,
    /// Canonical Mihomo executable inspected by the manager.
    pub binary: PathBuf,
    /// Human-readable platform evidence behind the state.
    pub detail: String,
}

/// Outcome of a user-requested TUN authorization operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TunPermissionGrant {
    /// Permission was installed and verified in the current process.
    Ready(TunPermissionStatus),
    /// The operating system accepted a request to relaunch `ZenClash` elevated.
    RelaunchRequested,
}

/// Inspects and explicitly grants privileges to one trusted runtime core.
#[derive(Clone, Debug)]
pub struct TunPermissionManager {
    binary: PathBuf,
}

impl TunPermissionManager {
    /// Creates a manager for a canonical, supported core executable.
    ///
    /// # Errors
    ///
    /// Returns an error when the path cannot be canonicalized, is not a regular
    /// file, or does not use a recognized runtime-core executable name.
    pub fn new(binary: impl AsRef<Path>) -> TunPermissionResult<Self> {
        let requested = binary.as_ref();
        let binary = std::fs::canonicalize(requested).map_err(|error| {
            TunPermissionError::InvalidBinary(format!("{}：{error}", requested.display()))
        })?;
        if !binary.is_file() {
            return Err(TunPermissionError::InvalidBinary(format!(
                "{} 不是普通文件",
                binary.display()
            )));
        }
        if !is_supported_core_name(&binary) {
            return Err(TunPermissionError::InvalidBinary(format!(
                "{} 不是受支持的 ZenClash 内核名称",
                binary.display()
            )));
        }
        Ok(Self { binary })
    }

    /// Reads the operating system's effective TUN privilege state.
    ///
    /// # Errors
    ///
    /// Returns an error when metadata or native privilege state cannot be read.
    pub fn status(&self) -> TunPermissionResult<TunPermissionStatus> {
        platform::status(&self.binary)
    }

    /// Opens the native authorization flow after an explicit user action.
    ///
    /// Unix platforms verify the executable owner and set-user-ID bit before
    /// returning. Windows asks the shell to relaunch the application elevated.
    ///
    /// # Errors
    ///
    /// Returns an error when the prompt is declined, the native operation
    /// fails, or Unix permission readback does not confirm the requested state.
    pub fn request_grant(&self) -> TunPermissionResult<TunPermissionGrant> {
        let current = self.status()?;
        if current.granted {
            return Ok(TunPermissionGrant::Ready(current));
        }
        platform::request_grant(&self.binary)
    }
}

fn is_supported_core_name(binary: &Path) -> bool {
    let Some(name) = binary.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "mihomo"
            | "mihomo.exe"
            | "mihomo-alpha"
            | "mihomo-alpha.exe"
            | "mihomo-smart"
            | "mihomo-smart.exe"
            | "meow"
            | "meow.exe"
    )
}

fn binary_sha256(binary: &Path) -> TunPermissionResult<String> {
    let mut file = File::open(binary).map_err(|error| {
        TunPermissionError::Platform(format!("无法读取 {}：{error}", binary.display()))
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            TunPermissionError::Platform(format!("读取 {} 失败：{error}", binary.display()))
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_recognized_core_names() {
        assert!(is_supported_core_name(Path::new("/tmp/mihomo")));
        assert!(is_supported_core_name(Path::new("C:/core/MIHOMO.EXE")));
        assert!(is_supported_core_name(Path::new("/tmp/mihomo-smart")));
        assert!(is_supported_core_name(Path::new("/tmp/meow")));
        assert!(is_supported_core_name(Path::new("C:/core/MEOW.EXE")));
        assert!(!is_supported_core_name(Path::new("/tmp/sh")));
        assert!(!is_supported_core_name(Path::new("/tmp/mihomo.backup")));
    }

    #[test]
    fn binary_digest_reads_the_exact_candidate_bytes() {
        let path = std::env::temp_dir().join(format!(
            "mihomo-digest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"zenclash-tun").unwrap();

        assert_eq!(
            binary_sha256(&path).unwrap(),
            "2a047459ab7474be50d2883326e1472b2e7587520b16854ced63d1404b307fe8"
        );
        std::fs::remove_file(path).unwrap();
    }
}
