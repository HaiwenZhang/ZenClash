//! PAC server I/O failures, retaining the failed operation and OS cause.

use std::io;
use thiserror::Error;

/// Operation that failed while starting a PAC listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PacServerOperation {
    /// Binding the HTTP listener.
    Bind,
    /// Configuring the listener for nonblocking access.
    ConfigureListener,
    /// Reading the listener's local address.
    ReadLocalAddress,
    /// Spawning the HTTP worker.
    SpawnWorker,
}

/// PAC listener or worker startup failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PacError {
    /// A PAC startup operation failed, retaining the OS cause.
    #[error("PAC 服务操作失败（{operation}）：{source}")]
    Io {
        /// Failed listener or worker operation.
        operation: PacServerOperation,
        /// Original operating-system failure.
        #[source]
        source: io::Error,
    },
}

impl std::fmt::Display for PacServerOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Bind => "绑定监听地址",
            Self::ConfigureListener => "配置监听器",
            Self::ReadLocalAddress => "读取监听地址",
            Self::SpawnWorker => "启动工作线程",
        })
    }
}
