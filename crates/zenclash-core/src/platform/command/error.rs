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
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "platform_command.errors.io",
            &[
                ("command", command.clone()),
                ("stage", stage.to_string()),
                ("error", source.to_string())
            ]
        )
    )]
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
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "platform_command.errors.timed_out",
            &[
                ("command", command.clone()),
                ("timeout", format!("{timeout:?}"))
            ]
        )
    )]
    TimedOut {
        /// Executable display name without arguments.
        command: String,
        /// Shared execution/output budget, including busy-executable retry time.
        timeout: Duration,
    },
    /// Child setup did not provide the requested output pipe.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "platform_command.errors.missing_pipe",
            &[
                ("command", command.clone()),
                ("stream", stream.to_string())
            ]
        )
    )]
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
        formatter.write_str(&zenclash_i18n::text(match self {
            Self::Spawn => "platform_command.stages.spawn",
            Self::ConfigureStdoutPipe => "platform_command.stages.configure_stdout_pipe",
            Self::ConfigureStderrPipe => "platform_command.stages.configure_stderr_pipe",
            Self::ReadStdout => "platform_command.stages.read_stdout",
            Self::ReadStderr => "platform_command.stages.read_stderr",
            Self::Wait => "platform_command.stages.wait",
        }))
    }
}
