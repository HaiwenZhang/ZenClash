//! Safe conversion of binary MRS rule providers through the real Mihomo executable.

use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

use crate::platform_command::output_path_with_timeout;

const CONVERSION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_MRS_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RULESET_TEXT_BYTES: usize = 64 * 1024 * 1024;
const TEMPORARY_ATTEMPTS: u8 = 32;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Rule-provider behavior accepted by Mihomo's `convert-ruleset` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RulesetBehavior {
    /// Domain suffix and exact-domain entries.
    #[default]
    Domain,
    /// IPv4 and IPv6 CIDR entries.
    IpCidr,
    /// Classical Clash rule expressions.
    Classical,
}

impl RulesetBehavior {
    /// Returns Mihomo's command-line spelling for this behavior.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Domain => "domain",
            Self::IpCidr => "ipcidr",
            Self::Classical => "classical",
        }
    }
}

/// Text produced by one successful MRS conversion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RulesetConversion {
    /// UTF-8 rule entries emitted by Mihomo.
    pub content: String,
    /// Size of the source MRS file.
    pub source_bytes: u64,
    /// Size of the converted UTF-8 payload.
    pub output_bytes: u64,
    /// Behavior used to decode the MRS trie.
    pub behavior: RulesetBehavior,
}

/// Errors returned by MRS validation, process execution, and bounded output reading.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RulesetConversionError {
    /// Filesystem access failed.
    #[error("规则集 I/O 错误：{0}")]
    Io(#[from] std::io::Error),
    /// The selected source is not a regular file.
    #[error("MRS 源文件不存在或不是普通文件：{0}")]
    InvalidSource(PathBuf),
    /// The source exceeds the defensive conversion limit.
    #[error("MRS 源文件超过 64 MiB 上限：{0} 字节")]
    SourceTooLarge(u64),
    /// Mihomo could not be executed or rejected the conversion.
    #[error("Mihomo 规则集转换失败：{0}")]
    Process(String),
    /// Converted text exceeds the defensive read limit.
    #[error("转换后的规则文本超过 64 MiB 上限")]
    OutputTooLarge,
    /// Mihomo emitted content that was not valid UTF-8.
    #[error("转换后的规则文本不是有效 UTF-8：{0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
    /// No collision-free temporary output could be reserved.
    #[error("无法创建安全的规则集转换临时文件")]
    TemporaryFileExhausted,
}

/// Result returned by [`RulesetConverter`].
pub type RulesetConversionResult<T> = Result<T, RulesetConversionError>;

/// Bounded wrapper around one trusted Mihomo executable.
#[derive(Clone, Debug)]
pub struct RulesetConverter {
    binary: PathBuf,
}

impl RulesetConverter {
    /// Creates a converter using an explicit Mihomo executable path.
    #[must_use]
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    /// Converts a local MRS file to UTF-8 rules using Mihomo itself.
    ///
    /// # Errors
    ///
    /// Returns an error when the source is absent or oversized, the child
    /// process fails or times out, or its output is oversized or invalid UTF-8.
    pub fn convert_mrs_to_text(
        &self,
        source: impl AsRef<Path>,
        behavior: RulesetBehavior,
    ) -> RulesetConversionResult<RulesetConversion> {
        let source = source.as_ref();
        let metadata = match fs::metadata(source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(RulesetConversionError::InvalidSource(source.to_path_buf()));
            }
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() {
            return Err(RulesetConversionError::InvalidSource(source.to_path_buf()));
        }
        if metadata.len() > MAX_MRS_SOURCE_BYTES {
            return Err(RulesetConversionError::SourceTooLarge(metadata.len()));
        }

        let target = TemporaryOutput::reserve("txt")?;
        let arguments = [
            OsStr::new("convert-ruleset"),
            OsStr::new(behavior.as_str()),
            OsStr::new("mrs"),
            source.as_os_str(),
            target.path().as_os_str(),
        ];
        let output = output_path_with_timeout(&self.binary, &arguments, CONVERSION_TIMEOUT)
            .map_err(RulesetConversionError::Process)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = stderr.trim();
            return Err(RulesetConversionError::Process(if message.is_empty() {
                format!("进程退出状态 {}", output.status)
            } else {
                message.to_owned()
            }));
        }

        let bytes = read_bounded(target.path())?;
        let output_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        Ok(RulesetConversion {
            content: String::from_utf8(bytes)?,
            source_bytes: metadata.len(),
            output_bytes,
            behavior,
        })
    }
}

fn read_bounded(path: &Path) -> RulesetConversionResult<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_RULESET_TEXT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_RULESET_TEXT_BYTES {
        return Err(RulesetConversionError::OutputTooLarge);
    }
    Ok(bytes)
}

struct TemporaryOutput {
    path: PathBuf,
}

impl TemporaryOutput {
    fn reserve(extension: &str) -> RulesetConversionResult<Self> {
        let directory = std::env::temp_dir();
        for _ in 0..TEMPORARY_ATTEMPTS {
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = directory.join(format!(
                "zenclash-ruleset-{}-{timestamp}-{sequence}.{extension}",
                std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(RulesetConversionError::TemporaryFileExhausted)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryOutput {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path) {
            if error.kind() == std::io::ErrorKind::NotFound {
                return;
            }
            tracing::debug!(%error, path = %self.path.display(), "failed to remove ruleset conversion temporary file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn behavior_arguments_match_mihomo_contract() {
        assert_eq!(RulesetBehavior::Domain.as_str(), "domain");
        assert_eq!(RulesetBehavior::IpCidr.as_str(), "ipcidr");
        assert_eq!(RulesetBehavior::Classical.as_str(), "classical");
    }

    #[test]
    fn converter_rejects_a_directory_before_process_execution() {
        let converter = RulesetConverter::new("missing-mihomo");
        let error = converter
            .convert_mrs_to_text(std::env::temp_dir(), RulesetBehavior::Domain)
            .unwrap_err();

        assert!(matches!(error, RulesetConversionError::InvalidSource(_)));
    }

    #[test]
    fn converter_reports_a_missing_source_without_running_mihomo() {
        let converter = RulesetConverter::new("missing-mihomo");
        let source = std::env::temp_dir().join(format!(
            "zenclash-missing-ruleset-{}.mrs",
            std::process::id()
        ));
        let error = converter
            .convert_mrs_to_text(source, RulesetBehavior::Domain)
            .unwrap_err();

        assert!(matches!(error, RulesetConversionError::InvalidSource(_)));
    }

    #[test]
    fn temporary_output_is_removed_on_drop() {
        let path = {
            let temporary = TemporaryOutput::reserve("txt").unwrap();
            temporary.path().to_path_buf()
        };

        assert!(!path.exists());
    }
}
