//! Failure facts retained by native command execution.

use std::{io, time::Duration};
use thiserror::Error;

/// The pipe whose capture or reader failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlatformCommandStream {
    /// Captured standard output.
    Stdout,
    /// Captured standard error.
    Stderr,
}

impl std::fmt::Display for PlatformCommandStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

/// The operation that produced a native command I/O failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlatformCommandStage {
    /// Creating the child process.
    Spawn,
    /// Configuring the exclusively owned standard-output pipe.
    ConfigureStdoutPipe,
    /// Configuring the exclusively owned standard-error pipe.
    ConfigureStderrPipe,
    /// Reading captured standard output.
    ReadStdout,
    /// Reading captured standard error.
    ReadStderr,
    /// Waiting for child termination.
    Wait,
}

/// A command failure retaining its stage and original I/O cause.
/// Command arguments and captured output are not included.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PlatformCommandError {
    /// A process or pipe operation failed.
    #[error("执行 {command} 失败（{stage}）：{source}")]
    Io {
        /// Executable display name without arguments.
        command: String,
        /// Failed process or pipe operation.
        stage: PlatformCommandStage,
        /// Original operating-system error.
        #[source]
        source: io::Error,
    },
    /// The child or output collection did not complete within the shared budget.
    #[error("执行 {command} 超时（{timeout:?}），已停止收集")]
    TimedOut {
        /// Executable display name without arguments.
        command: String,
        /// Shared execution/output budget, including busy-executable retry time.
        timeout: Duration,
    },
    /// Child setup did not provide the requested output pipe.
    #[error("执行 {command} 失败：无法捕获 {stream}")]
    MissingPipe {
        /// Executable display name without arguments.
        command: String,
        /// Requested pipe.
        stream: PlatformCommandStream,
    },
}

impl PlatformCommandError {
    pub(super) fn io(command: &str, stage: PlatformCommandStage, source: io::Error) -> Self {
        Self::Io {
            command: command.into(),
            stage,
            source,
        }
    }
}

impl std::fmt::Display for PlatformCommandStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Spawn => "启动进程",
            Self::ConfigureStdoutPipe => "配置标准输出管道",
            Self::ConfigureStderrPipe => "配置标准错误管道",
            Self::ReadStdout => "读取标准输出",
            Self::ReadStderr => "读取标准错误",
            Self::Wait => "等待进程结束",
        })
    }
}
