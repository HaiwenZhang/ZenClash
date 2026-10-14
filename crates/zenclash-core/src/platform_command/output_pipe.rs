//! One exclusive child-output reader with bounded, nonblocking collection.

use super::MAX_COMMAND_OUTPUT_BYTES;
use std::io::{self, Read};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

#[cfg(unix)]
pub(super) trait CommandPipe: Read + AsRawFd {}
#[cfg(unix)]
impl<T: Read + AsRawFd> CommandPipe for T {}
#[cfg(windows)]
pub(super) trait CommandPipe: Read + AsRawHandle {}
#[cfg(windows)]
impl<T: Read + AsRawHandle> CommandPipe for T {}
#[cfg(not(any(unix, windows)))]
pub(super) trait CommandPipe: Read {}
#[cfg(not(any(unix, windows)))]
impl<T: Read> CommandPipe for T {}

pub(super) struct CommandOutputPipe<P> {
    pipe: P,
    bytes: Vec<u8>,
    complete: bool,
}

impl<P: CommandPipe> CommandOutputPipe<P> {
    pub(super) fn new(pipe: P) -> io::Result<Self> {
        prepare_pipe(&pipe)?;
        Ok(Self {
            pipe,
            bytes: Vec::new(),
            complete: false,
        })
    }

    /// Reads at most one chunk so a busy stream cannot starve the other or the deadline.
    pub(super) fn poll(&mut self) -> io::Result<bool> {
        if self.complete {
            return Ok(false);
        }
        let mut chunk = [0_u8; 8192];
        match read_available(&mut self.pipe, &mut chunk) {
            Ok(0) => {
                self.complete = true;
                Ok(true)
            }
            Ok(length) => {
                if length > MAX_COMMAND_OUTPUT_BYTES.saturating_sub(self.bytes.len()) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "命令输出超过 1 MiB 上限",
                    ));
                }
                self.bytes.extend_from_slice(&chunk[..length]);
                Ok(true)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn is_complete(&self) -> bool {
        self.complete
    }
    pub(super) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(unix)]
fn prepare_pipe(pipe: &impl CommandPipe) -> io::Result<()> {
    // SAFETY: This descriptor belongs to the exclusively owned child output pipe.
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Preserve existing flags and make reads report WouldBlock instead of waiting for EOF.
    if unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn read_available(pipe: &mut impl CommandPipe, bytes: &mut [u8]) -> io::Result<usize> {
    pipe.read(bytes)
}

#[cfg(windows)]
fn prepare_pipe(_pipe: &impl CommandPipe) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn read_available(pipe: &mut impl CommandPipe, bytes: &mut [u8]) -> io::Result<usize> {
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED},
        System::Pipes::PeekNamedPipe,
    };
    let mut available = 0;
    // SAFETY: This is the sole reader of the live ChildStdout/ChildStderr handle.
    // It never has another pending read. Peek does not consume buffered data.
    let success = unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle().cast(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            &raw mut available,
            ptr::null_mut(),
        )
    };
    if success == 0 {
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(code) if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32)
        {
            return Ok(0);
        }
        return Err(error);
    }
    if available == 0 {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    // Only this reader consumes data, so a read bounded by Peek's byte count cannot await new bytes.
    let length = bytes.len().min(available as usize);
    pipe.read(&mut bytes[..length])
}

#[cfg(not(any(unix, windows)))]
fn prepare_pipe(_pipe: &impl CommandPipe) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}
#[cfg(not(any(unix, windows)))]
fn read_available(_pipe: &mut impl CommandPipe, _bytes: &mut [u8]) -> io::Result<usize> {
    Err(io::ErrorKind::Unsupported.into())
}
