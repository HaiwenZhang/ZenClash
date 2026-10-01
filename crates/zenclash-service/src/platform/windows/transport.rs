use std::{io, os::windows::io::AsRawHandle, ptr};

use tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(feature = "server")]
use tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(any(test, feature = "server"))]
use tokio::net::windows::named_pipe::ServerOptions;
#[cfg(feature = "server")]
use windows_sys::Win32::System::{
    Pipes::{GetNamedPipeClientProcessId, GetNamedPipeClientSessionId},
    RemoteDesktop::ProcessIdToSessionId,
};
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, SECURITY_IDENTIFICATION,
        SECURITY_SQOS_PRESENT,
    },
    System::Pipes::GetNamedPipeServerProcessId,
};

#[cfg(feature = "server")]
use super::identity::process_identity;
#[cfg(any(test, feature = "server"))]
use super::security::Descriptor;
use super::{
    PIPE_NAME, denied,
    identity::{process_handle, process_image},
    scm::service_pid,
    security::{service_root, validate_protected_path},
    wide,
};
#[cfg(feature = "server")]
use crate::session::PeerIdentity;

pub(crate) type Stream = NamedPipeClient;

const CLIENT_ACCESS: u32 = 0x0012_008b;
#[cfg(feature = "server")]
const PIPE_SDDL: &str = "O:SYG:SYD:P(A;;GA;;;SY)(A;;0x12008b;;;AU)";

pub(crate) async fn connect() -> io::Result<Stream> {
    let stream = open_client(PIPE_NAME)?;
    validate_server(&stream)?;
    Ok(stream)
}

fn open_client(name: &str) -> io::Result<NamedPipeClient> {
    let path = wide(name)?;
    // Exclude FILE_CREATE_PIPE_INSTANCE: generic write would grant the caller
    // permission to create a competing server instance of the same pipe.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            CLIENT_ACCESS,
            0,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // Tokio takes ownership even when IOCP registration fails.
    unsafe { NamedPipeClient::from_raw_handle(handle.cast()) }
}

fn validate_server(stream: &NamedPipeClient) -> io::Result<()> {
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(stream.as_raw_handle().cast(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if service_pid()? != Some(pid) {
        return Err(denied("named pipe is not owned by the registered service"));
    }
    let process = process_handle(pid)?;
    let identity = super::identity::identity_from_handle(pid, process.0)?;
    if identity.user() != "S-1-5-18" {
        return Err(denied("service is not running as LocalSystem"));
    }
    let root = service_root()?;
    validate_protected_path(&root, true)?;
    let expected = root.join("zenclash-service.exe");
    validate_protected_path(&expected, false)?;
    if process_image(process.0)?.to_string_lossy().to_lowercase()
        != expected.to_string_lossy().to_lowercase()
    {
        return Err(denied(
            "service image does not match the protected installation",
        ));
    }
    Ok(())
}

#[cfg(feature = "server")]
pub(crate) struct Listener {
    next: NamedPipeServer,
}

#[cfg(feature = "server")]
impl Listener {
    pub(crate) fn bind() -> io::Result<Self> {
        if super::identity::current_identity()?.user() != "S-1-5-18" {
            return Err(denied("Windows service listener requires LocalSystem"));
        }
        Ok(Self {
            next: create_pipe(true)?,
        })
    }

    pub(crate) async fn accept(&mut self) -> io::Result<(NamedPipeServer, PeerIdentity)> {
        self.next.connect().await?;
        // Keep an instance registered while constructing the next, so an attacker
        // cannot reserve the name between legitimate connections.
        let next = create_pipe(false)?;
        let stream = std::mem::replace(&mut self.next, next);
        let raw = stream.as_raw_handle().cast();
        let mut pid = 0;
        let mut pipe_session = 0;
        let mut process_session = 0;
        if unsafe { GetNamedPipeClientProcessId(raw, &mut pid) } == 0
            || unsafe { GetNamedPipeClientSessionId(raw, &mut pipe_session) } == 0
            || unsafe { ProcessIdToSessionId(pid, &mut process_session) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if pipe_session != process_session {
            return Err(denied("named pipe client session mismatch"));
        }
        let identity = process_identity(pid)?;
        Ok((stream, identity))
    }
}

#[cfg(feature = "server")]
fn create_pipe(first: bool) -> io::Result<NamedPipeServer> {
    let security = Descriptor::from_sddl(PIPE_SDDL)?;
    let mut attributes = security.attributes();
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .max_instances(17)
            .create_with_security_attributes_raw(
                PIPE_NAME,
                (&mut attributes as *mut windows_sys::Win32::Security::SECURITY_ATTRIBUTES).cast(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unprivileged_pipe_impostor_cannot_be_accepted_as_service() {
        let name = format!(r"\\.\pipe\ZenClash.Impostor.{}", std::process::id());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let stream = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&name)
            .unwrap();
        server.connect().await.unwrap();
        assert_eq!(
            validate_server(&stream).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[tokio::test]
    async fn explicit_pipe_rights_allow_duplex_io_without_allowing_server_impersonation() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let user = super::super::identity::current_identity().unwrap();
        let descriptor = Descriptor::from_sddl(&format!(
            "O:{}D:P(A;;0x12008b;;;{})",
            user.user(),
            user.user()
        ))
        .unwrap();
        let mut attributes = descriptor.attributes();
        let name = format!(r"\\.\pipe\ZenClash.Rights.{}", std::process::id());
        let mut server = unsafe {
            ServerOptions::new()
                .first_pipe_instance(true)
                .create_with_security_attributes_raw(
                    &name,
                    (&mut attributes as *mut windows_sys::Win32::Security::SECURITY_ATTRIBUTES)
                        .cast(),
                )
        }
        .unwrap();
        let mut client = open_client(&name).unwrap();
        server.connect().await.unwrap();
        client.write_all(b"request").await.unwrap();
        let mut request = [0; 7];
        server.read_exact(&mut request).await.unwrap();
        assert_eq!(&request, b"request");
        server.write_all(b"response").await.unwrap();
        let mut response = [0; 8];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(&response, b"response");
        assert!(ServerOptions::new().create(&name).is_err());
    }
}
