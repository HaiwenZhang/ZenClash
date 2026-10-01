//! Root-owned Unix transport and native service management.

use std::{
    fs, io,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::net::UnixStream;

#[cfg(feature = "server")]
use std::os::unix::fs::PermissionsExt;

#[cfg(feature = "server")]
#[path = "unix/child.rs"]
mod child;
#[cfg(feature = "server")]
pub(crate) use child::{spawn as spawn_core, spawn_validation};

#[cfg(target_os = "linux")]
#[path = "unix/linux.rs"]
mod linux;
#[cfg(target_os = "macos")]
#[path = "unix/macos.rs"]
mod macos;
#[cfg(target_os = "linux")]
use linux as native;
#[cfg(target_os = "macos")]
use macos as native;

pub(crate) type LocalStream = UnixStream;

#[cfg(feature = "server")]
pub(crate) fn service_root() -> PathBuf {
    native::service_root().into()
}

pub(crate) fn socket_path() -> PathBuf {
    native::socket_path().into()
}

#[cfg(feature = "server")]
fn current_user() -> String {
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    unsafe { libc::geteuid() }.to_string()
}

pub(crate) fn current_identity() -> io::Result<crate::session::PeerIdentity> {
    let pid = std::process::id();
    let (uid, birth) = native::process_identity(pid)?;
    Ok(crate::session::PeerIdentity::new(
        uid.to_string(),
        pid,
        birth,
    ))
}

#[cfg(feature = "server")]
pub(crate) fn verified_owner(pid: u32, birth: u64) -> io::Result<crate::session::PeerIdentity> {
    let (uid, actual_birth) = native::process_identity(pid)?;
    if birth == 0 || actual_birth != birth {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "maintenance owner identity changed",
        ));
    }
    Ok(crate::session::PeerIdentity::new(
        uid.to_string(),
        pid,
        birth,
    ))
}

#[cfg(feature = "server")]
pub(crate) fn verify_kernel_peer(stream: &UnixStream, expected_pid: u32) -> io::Result<()> {
    let (uid, pid, birth) = native::authenticate_peer(stream)?;
    if uid != 0 || pid != expected_pid || !native::process_matches(pid, uid, birth) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "kernel controller peer identity does not match the launched process",
        ));
    }
    Ok(())
}

pub(crate) fn open_pinned_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "approved artifact is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(feature = "server")]
pub(crate) fn create_private_directory(path: &Path, public_read: bool) -> io::Result<()> {
    // macOS does not create PrivilegedHelperTools on all installations.
    #[cfg(target_os = "macos")]
    if path == service_root() && !Path::new("/Library/PrivilegedHelperTools").exists() {
        create_protected_directory(Path::new("/Library/PrivilegedHelperTools"), 0o755)?;
    }
    create_protected_directory(path, if public_read { 0o755 } else { 0o700 })
}

#[cfg(feature = "server")]
pub(crate) fn require_admin() -> io::Result<()> {
    if current_user() != "0" {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "administrator authorization required",
        ));
    }
    Ok(())
}

/// Checks every ancestor without following symlinks, including the leaf.
pub(crate) fn validate_protected_path(path: &Path, is_dir: bool) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "protected path must be absolute",
        ));
    }
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if metadata.file_type().is_symlink() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "service path is not root protected",
            ));
        }
        #[cfg(target_os = "macos")]
        native::validate_acl(ancestor)?;
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() != is_dir || (!is_dir && !metadata.is_file()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid protected path kind",
        ));
    }
    Ok(())
}

#[cfg(feature = "server")]
pub(crate) fn create_protected_directory(path: &Path, mode: u32) -> io::Result<()> {
    require_admin()?;
    if !path.exists() {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
        validate_protected_path(parent, true)?;
        match fs::create_dir(path) {
            Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(mode))?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error),
        }
    }
    validate_protected_path(path, true)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "service directory is not a directory",
        ));
    }
    Ok(())
}

pub(crate) async fn connect() -> io::Result<LocalStream> {
    // Metadata checks are synchronous and must run on the background client task.
    let path = socket_path();
    validate_protected_path(
        path.parent()
            .ok_or_else(|| io::Error::other("missing socket parent"))?,
        true,
    )?;
    if !fs::symlink_metadata(&path)?.file_type().is_socket()
        || fs::symlink_metadata(&path)?.uid() != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "service endpoint is not a socket",
        ));
    }
    let stream = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(&path)).await??;
    let (uid, pid, birth) = native::authenticate_peer(&stream)?;
    if uid != 0 || !native::process_matches(pid, uid, birth) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "service peer is not a live root process",
        ));
    }
    validate_protected_path(
        path.parent()
            .ok_or_else(|| io::Error::other("missing socket parent"))?,
        true,
    )?;
    Ok(stream)
}

#[cfg(feature = "server")]
pub(crate) struct Listener {
    inner: tokio::net::UnixListener,
    identity: (u64, u64),
}

#[cfg(feature = "server")]
impl Listener {
    pub(crate) fn bind() -> io::Result<Self> {
        require_admin()?;
        native::prepare_socket_directory()?;
        let path = socket_path();
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if !metadata.file_type().is_socket() || metadata.uid() != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unexpected endpoint file",
                ));
            }
            // Never unlink a live listener. Only ECONNREFUSED proves a stale endpoint.
            if endpoint_is_stale(&path)? {
                fs::remove_file(&path)?;
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "service already listening",
                ));
            }
        }
        let inner = tokio::net::UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666))?;
        let metadata = fs::symlink_metadata(path)?;
        Ok(Self {
            inner,
            identity: (metadata.dev(), metadata.ino()),
        })
    }

    pub(crate) async fn accept(&self) -> io::Result<(LocalStream, crate::session::PeerIdentity)> {
        let (stream, _) = self.inner.accept().await?;
        let (uid, pid, birth) = native::authenticate_peer(&stream)?;
        if !native::process_matches(pid, uid, birth) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "peer process identity changed",
            ));
        }
        Ok((
            stream,
            crate::session::PeerIdentity::new(uid.to_string(), pid, birth),
        ))
    }
}

#[cfg(feature = "server")]
impl Drop for Listener {
    fn drop(&mut self) {
        let path = socket_path();
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && metadata.file_type().is_socket()
            && (metadata.dev(), metadata.ino()) == self.identity
        {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(feature = "server")]
pub(crate) fn peer_alive(peer: &crate::session::PeerIdentity) -> bool {
    let Ok(uid) = peer.user().parse::<u32>() else {
        return false;
    };
    native::process_matches(peer.pid(), uid, peer.birth())
}

#[cfg(feature = "server")]
pub(crate) async fn shutdown_signal() -> io::Result<()> {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    tokio::select! { _ = term.recv() => (), _ = interrupt.recv() => () }
    Ok(())
}

pub(crate) async fn request_maintenance(
    action: &str,
    helper: &Path,
    arguments: &[std::ffi::OsString],
) -> io::Result<()> {
    if !matches!(action, "install" | "repair" | "uninstall" | "start") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown service maintenance operation",
        ));
    }
    #[cfg(target_os = "macos")]
    validate_protected_path(Path::new("/Library"), true)?;
    #[cfg(target_os = "linux")]
    validate_protected_path(Path::new("/var/lib"), true)?;
    #[cfg(target_os = "linux")]
    if helper == Path::new("/usr/lib/zenclash/zenclash-service")
        && validate_protected_path(helper, false).is_ok()
    {
        let mut authorized = vec![
            helper.as_os_str().to_owned(),
            std::ffi::OsString::from(format!("--{action}")),
        ];
        authorized.extend_from_slice(arguments);
        return native::request_maintenance(&authorized).await;
    }
    let sha = arguments
        .windows(2)
        .find(|pair| pair[0] == "--helper-sha256")
        .map(|pair| &pair[1])
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "missing approved helper digest",
            )
        })?;
    let mut authorized = vec![
        std::ffi::OsString::from("/bin/sh"),
        std::ffi::OsString::from("-c"),
        std::ffi::OsString::from(native::BOOTSTRAP_SCRIPT),
        std::ffi::OsString::from("zenclash-bootstrap"),
        helper.as_os_str().to_owned(),
        sha.clone(),
        std::ffi::OsString::from(format!("--{action}")),
    ];
    authorized.extend_from_slice(arguments);
    native::request_maintenance(&authorized).await
}

#[cfg(feature = "server")]
pub(crate) fn register_service() -> io::Result<()> {
    native::register_service()
}
#[cfg(feature = "server")]
pub(crate) fn start_service() -> io::Result<()> {
    native::start_service()
}
#[cfg(feature = "server")]
pub(crate) fn stop_service() -> io::Result<()> {
    native::stop_service()
}
#[cfg(feature = "server")]
pub(crate) fn unregister_service() -> io::Result<()> {
    native::unregister_service()
}

#[cfg(feature = "server")]
pub(crate) fn service_was_running() -> io::Result<bool> {
    // The verified endpoint proves a live root-owned daemon process without
    // acquiring another user's session or requiring protocol compatibility.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    match runtime.block_on(connect()) {
        Ok(_) => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(feature = "server")]
pub(super) fn run_native(program: &str, args: &[&str]) -> io::Result<()> {
    let status = run_native_status(program, args)?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "native service operation failed: {program} ({status})"
        )))
    }
}

#[cfg(feature = "server")]
pub(super) fn run_native_status(
    program: &str,
    args: &[&str],
) -> io::Result<std::process::ExitStatus> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => (),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native service operation timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(feature = "server")]
pub(crate) fn write_registration(temp: &Path, target: &Path, content: &[u8]) -> io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    if temp.exists() {
        validate_protected_path(temp, false)?;
        fs::remove_file(temp)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(temp)?;
    let result = (|| {
        file.write_all(content)?;
        file.sync_all()?;
        fs::rename(temp, target)?;
        fs::File::open(
            target
                .parent()
                .ok_or_else(|| io::Error::other("missing registration parent"))?,
        )?
        .sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(feature = "server")]
fn endpoint_is_stale(path: &Path) -> io::Result<bool> {
    use std::{
        mem,
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::ffi::OsStrExt,
        },
    };
    // A blocking connect can hang when a live listener's backlog is full.
    // SAFETY: socket arguments are native constants, no borrowed pointers.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socket returned an owned, valid descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: fd is live and fcntl's arguments match each command.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: all-zero sockaddr_un is a valid initialized storage value.
    let mut address: libc::sockaddr_un = unsafe { mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path too long",
        ));
    }
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    #[cfg(target_os = "macos")]
    {
        address.sun_len = mem::size_of::<libc::sockaddr_un>() as u8;
    }
    // SAFETY: address is initialized, sized to the advertised native struct.
    let result = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            mem::size_of_val(&address) as libc::socklen_t,
        )
    };
    if result == 0 {
        return Ok(false);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ECONNREFUSED) => Ok(true),
        Some(libc::EINPROGRESS) | Some(libc::EAGAIN) | Some(libc::EALREADY) => Ok(false),
        _ => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn kernel_peer_requires_the_expected_process_and_root_credentials() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let actual = std::process::id();
        let wrong = actual.checked_add(1).unwrap_or(1);
        assert_eq!(
            verify_kernel_peer(&stream, wrong).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let verified = verify_kernel_peer(&stream, actual);
        // SAFETY: geteuid has no preconditions or pointer arguments.
        if unsafe { libc::geteuid() } == 0 {
            assert!(verified.is_ok());
        } else {
            assert_eq!(
                verified.unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
    }

    #[test]
    fn user_writable_parent_cannot_be_a_service_location() {
        let path = std::env::temp_dir().join(format!("zenclash-path-test-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        assert!(validate_protected_path(&path, true).is_err());
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn relative_paths_are_rejected_before_inspecting_files() {
        assert_eq!(
            validate_protected_path(Path::new("../zenclash-service"), true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
