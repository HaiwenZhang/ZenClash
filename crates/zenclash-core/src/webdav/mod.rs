//! `WebDAV` storage for versioned `ZenClash` backup archives.

use thiserror::Error;

use crate::BackupError;

mod model;
mod service;
mod storage;

#[cfg(test)]
mod tests;

pub use model::{WebDavBackup, WebDavSettings, WebDavUploadSummary};
pub use service::WebDavService;
pub use storage::WebDavSettingsStore;

/// Errors produced by `WebDAV` settings, protocol, and backup operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WebDavError {
    /// `WebDAV` settings are incomplete or unsafe.
    #[error("WebDAV 设置无效：{0}")]
    InvalidSettings(String),
    /// A caller supplied an unsafe remote backup filename.
    #[error("WebDAV 备份文件名无效：{0}")]
    InvalidFilename(String),
    /// Local filesystem access failed.
    #[error("WebDAV 本地 I/O 错误：{0}")]
    Io(#[from] std::io::Error),
    /// Settings JSON could not be encoded or decoded.
    #[error("WebDAV 设置 JSON 无效：{0}")]
    Json(#[from] serde_json::Error),
    /// The HTTP transport failed before a usable response was received.
    #[error("WebDAV 网络请求失败：{0}")]
    Http(#[from] reqwest::Error),
    /// A `WebDAV` XML response was malformed.
    #[error("WebDAV XML 响应无效：{0}")]
    Xml(String),
    /// The `WebDAV` server rejected an operation.
    #[error("WebDAV {method} 返回 HTTP {status}：{message}")]
    Status {
        /// HTTP/WebDAV method name.
        method: String,
        /// Numeric HTTP status code.
        status: u16,
        /// Bounded response detail.
        message: String,
    },
    /// A response exceeded its defensive in-memory limit.
    #[error("WebDAV 响应超过安全大小限制：{0}")]
    ResponseTooLarge(String),
    /// A configured backup cron expression is invalid or has no future run.
    #[error("WebDAV 定时计划无效：{0}")]
    Schedule(String),
    /// A blocking worker ended unexpectedly.
    #[error("WebDAV 后台任务异常结束：{0}")]
    Task(String),
    /// The local backup archive was invalid or could not be activated.
    #[error("WebDAV 备份处理失败：{0}")]
    Backup(#[from] BackupError),
}

/// Result type returned by `WebDAV` operations.
pub type WebDavResult<T> = Result<T, WebDavError>;
