#[cfg(any(feature = "server", test))]
use std::fs;
#[cfg(test)]
use std::time::Duration;
use std::{io, mem, os::fd::AsRawFd, path::Path, process::Stdio};

use tokio::{net::UnixStream, process::Command};

// Native ACL APIs predate the project's minimum macOS version. Darwin ACLs
// can grant write access independently of POSIX mode bits, so inspect both.
unsafe extern "C" {
    fn acl_get_file(path: *const libc::c_char, kind: libc::c_int) -> *mut libc::c_void;
    fn acl_get_entry(
        acl: *mut libc::c_void,
        index: libc::c_int,
        entry: *mut *mut libc::c_void,
    ) -> libc::c_int;
    fn acl_get_tag_type(entry: *mut libc::c_void, tag: *mut libc::c_int) -> libc::c_int;
    fn acl_get_permset_mask_np(entry: *mut libc::c_void, mask: *mut u64) -> libc::c_int;
    fn acl_free(value: *mut libc::c_void) -> libc::c_int;
}

pub(super) fn validate_acl(path: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: C string is valid for the call; acl_get_file returns an owned ACL.
    let acl = unsafe { acl_get_file(path.as_ptr(), 0x100) };
    if acl.is_null() {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut index = 0;
        loop {
            let mut entry = std::ptr::null_mut();
            // SAFETY: acl is live, and entry points to correctly sized output.
            let found = unsafe { acl_get_entry(acl, index, &mut entry) };
            if found != 0 {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(libc::EINVAL) {
                    Ok(())
                } else {
                    Err(error)
                };
            }
            index = -1;
            let mut tag = 0;
            let mut mask = 0;
            // SAFETY: entry belongs to the live ACL; tag and mask are ABI outputs.
            if unsafe { acl_get_tag_type(entry, &mut tag) } != 0
                || unsafe { acl_get_permset_mask_np(entry, &mut mask) } != 0
            {
                return Err(io::Error::last_os_error());
            }
            const MUTATION: u64 = (1 << 2)
                | (1 << 4)
                | (1 << 5)
                | (1 << 6)
                | (1 << 8)
                | (1 << 10)
                | (1 << 12)
                | (1 << 13);
            if tag == 1 && mask & MUTATION != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "service ancestor ACL grants mutation access",
                ));
            }
        }
    })();
    // SAFETY: frees the owned ACL once after entry traversal ends.
    unsafe { acl_free(acl) };
    result
}

pub(super) fn service_root() -> &'static str {
    "/Library/PrivilegedHelperTools/org.zenclash.service"
}
pub(super) fn socket_path() -> &'static str {
    "/Library/PrivilegedHelperTools/org.zenclash.service/control.sock"
}

pub(super) fn process_identity(pid: u32) -> io::Result<(u32, u64)> {
    let pid = i32::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid process ID"))?;
    let mut info = mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    // SAFETY: info points to initialized storage of the exact ABI type and size.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            mem::size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    if written != mem::size_of::<libc::proc_bsdinfo>() as i32 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "process identity unavailable",
        ));
    }
    // SAFETY: proc_pidinfo reported a complete initialized proc_bsdinfo.
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid as u32 || info.pbi_status == libc::SZOMB {
        return Err(io::Error::new(io::ErrorKind::NotFound, "process ended"));
    }
    let birth = info
        .pbi_start_tvsec
        .checked_mul(1_000_000)
        .and_then(|secs| secs.checked_add(info.pbi_start_tvusec))
        .filter(|birth| *birth > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid process birth"))?;
    Ok((info.pbi_uid, birth))
}

pub(super) fn process_matches(pid: u32, uid: u32, birth: u64) -> bool {
    process_identity(pid).is_ok_and(|identity| identity == (uid, birth))
}

#[cfg(feature = "server")]
pub(super) fn process_image(pid: u32) -> io::Result<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let pid = i32::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid process PID"))?;
    let mut buffer = vec![0u8; 4096];
    // SAFETY: proc_pidpath receives a valid owned output buffer and its byte length.
    let length =
        unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if length <= 0 {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(length as usize);
    if buffer.last() == Some(&0) {
        buffer.pop();
    }
    Ok(std::ffi::OsString::from_vec(buffer).into())
}

#[cfg(feature = "server")]
pub(super) fn maintenance_host_registered(pid: u32) -> io::Result<()> {
    let helper = super::service_root().join("zenclash-service");
    super::validate_protected_path(&helper, false)?;
    let _pinned = super::open_pinned_file(&helper)?;
    let mut command = tokio::process::Command::new(&helper);
    command
        .env_clear()
        .env("LC_ALL", "C")
        .arg("--query-host-pid");
    let actual =
        super::super::launchd_probe::capture_host_pid(command, std::time::Duration::from_secs(5))?;
    super::validate_protected_path(&helper, false)?;
    if actual != pid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "loaded job host PID changed",
        ));
    }
    Ok(())
}

#[cfg(feature = "server")]
pub(super) fn query_loaded_host_pid() -> io::Result<u32> {
    super::super::macos_job::query_fixed_host_pid()
}

#[cfg(feature = "server")]
pub(super) fn maintenance_service_absent() -> io::Result<()> {
    use super::super::launchd_probe::{
        LaunchdServiceState, classify_launchd_service_probe, probe_service,
    };
    let output = probe_service(LABEL)?;
    if classify_launchd_service_probe(output.code, &output.diagnostic)?
        != LaunchdServiceState::Absent
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "service job remains loaded",
        ));
    }
    Ok(())
}

pub(super) fn authenticate_peer(stream: &UnixStream) -> io::Result<(u32, u32, u64)> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: getpeereid receives a live socket and valid UID/GID output storage.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut pid: libc::pid_t = 0;
    let mut len = mem::size_of_val(&pid) as libc::socklen_t;
    // SOL_LOCAL=0 is the ABI level for LOCAL_PEERPID on Darwin.
    // SAFETY: pid storage and its advertised size match the socket option ABI.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            0,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut len,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if len as usize != mem::size_of_val(&pid) || pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket peer PID unavailable",
        ));
    }
    let (actual_uid, birth) = process_identity(pid as u32)?;
    if actual_uid != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket peer UID changed",
        ));
    }
    Ok((uid, pid as u32, birth))
}

#[cfg(feature = "server")]
pub(super) fn prepare_socket_directory() -> io::Result<()> {
    super::create_protected_directory(Path::new(service_root()), 0o755)
}

const AUTHORIZATION_SCRIPT: &str = concat!(
    "on run argv\n",
    "set commandText to \"\"\n",
    "repeat with argument in argv\n",
    "set commandText to commandText & quoted form of (argument as text) & \" \"\n",
    "end repeat\n",
    "try\n",
    "do shell script commandText with administrator privileges\n",
    "on error messageText number errorNumber\n",
    "if errorNumber is -128 then error \"ZENCLASH_AUTHORIZATION_CANCELLED\" number -128\n",
    "error \"ZenClash service maintenance failed\" number errorNumber\n",
    "end try\n",
    "end run"
);

pub(super) const BOOTSTRAP_SCRIPT: &str = concat!(
    "set -eu\n",
    "source=$1; expected=$2; shift 2\n",
    "umask 077\n",
    "temporary=$(/usr/bin/mktemp -d /Library/.zenclash-bootstrap-XXXXXXXX)\n",
    "trap '/bin/rm -f \"$temporary/helper\"; /bin/rmdir \"$temporary\"' EXIT\n",
    // macOS ships shasum and its Perl/Digest::SHA runtime. Open the source
    // nonblocking and without following its leaf link, then validate the actual
    // descriptor, avoiding cp/dd's regular-file-to-FIFO race after stat.
    "/usr/bin/perl -e 'use strict; use warnings; use Fcntl qw(O_RDONLY O_WRONLY O_CREAT O_EXCL O_NOFOLLOW O_NONBLOCK); use Digest::SHA; ",
    "sysopen(my $input, $ARGV[0], O_RDONLY|O_NOFOLLOW|O_NONBLOCK) or die; -f $input or die; ",
    "my $size=(stat($input))[7]; $size>0 && $size<=268435456 or die; ",
    "sysopen(my $output, $ARGV[1], O_WRONLY|O_CREAT|O_EXCL, 0700) or die; ",
    "my $hash=Digest::SHA->new(256); my $total=0; my $buffer; ",
    "while (1) {my $read=sysread($input,$buffer,65536); defined($read) or die; last if $read==0; ",
    "$total+=$read; $total<=268435456 or die; $hash->add($buffer); my $offset=0; ",
    "while($offset<$read){my $written=syswrite($output,$buffer,$read-$offset,$offset); defined($written)&&$written>0 or die; $offset+=$written;}} ",
    "$total>0 && $hash->hexdigest eq $ARGV[2] or die; close($input) or die; close($output) or die;' -- \"$source\" \"$temporary/helper\" \"$expected\"\n",
    "/bin/chmod 500 \"$temporary/helper\"\n",
    "\"$temporary/helper\" \"$@\"\n"
);

pub(super) async fn request_maintenance(arguments: &[std::ffi::OsString]) -> io::Result<()> {
    if !Path::new("/usr/bin/perl").is_file() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "macOS secure service installation requires the system Perl runtime used by shasum",
        ));
    }
    if arguments.iter().any(|argument| argument.to_str().is_none()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "macOS authorization requires Unicode paths",
        ));
    }
    let mut child = Command::new("/usr/bin/osascript")
        .args(["-e", AUTHORIZATION_SCRIPT])
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(false)
        .spawn()?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("authorization diagnostic unavailable"))?;
    // Read a bounded diagnostic only; never include helper arguments or output
    // in public error text because maintenance arguments can contain metadata.
    let diagnostics = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut content = Vec::new();
        stderr.take(4096).read_to_end(&mut content).await?;
        Ok::<_, io::Error>(content)
    });
    let status = child.wait().await.map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "authorized maintenance exit could not be observed",
        )
    })?;
    let diagnostic = diagnostics.await.map_err(io::Error::other)??;
    if status.success() {
        Ok(())
    } else if diagnostic
        .windows(b"ZENCLASH_AUTHORIZATION_CANCELLED".len())
        .any(|part| part == b"ZENCLASH_AUTHORIZATION_CANCELLED")
    {
        Err(crate::installer::authorization_cancelled())
    } else {
        Err(io::Error::other(
            "administrator-authorized service maintenance failed",
        ))
    }
}

#[cfg(feature = "server")]
const PLIST_PATH: &str = "/Library/LaunchDaemons/org.zenclash.service.plist";
#[cfg(feature = "server")]
const LABEL: &str = "system/org.zenclash.service";

#[cfg(feature = "server")]
pub(super) fn validate_service_registration() -> io::Result<()> {
    super::super::macos_registration::validate_registration(
        Path::new(PLIST_PATH),
        &super::validate_protected_path,
    )
}

#[cfg(feature = "server")]
pub(super) fn register_service() -> io::Result<()> {
    super::require_admin()?;
    super::validate_protected_path(Path::new("/Library/LaunchDaemons"), true)?;
    super::super::macos_registration::with_known_registration(
        Path::new(PLIST_PATH),
        &super::validate_protected_path,
        || {
            super::write_registration(
                Path::new("/Library/LaunchDaemons/.org.zenclash.service.plist.new"),
                Path::new(PLIST_PATH),
                super::super::macos_registration::PLIST.as_bytes(),
            )
        },
    )
}

#[cfg(feature = "server")]
pub(super) fn start_service() -> io::Result<()> {
    maintain_service(super::super::launchd_probe::MaintenanceAction::Start)
}

#[cfg(feature = "server")]
pub(super) fn stop_service() -> io::Result<()> {
    maintain_service(super::super::launchd_probe::MaintenanceAction::Stop)
}

#[cfg(feature = "server")]
pub(super) fn unregister_service() -> io::Result<()> {
    maintain_service(super::super::launchd_probe::MaintenanceAction::Unregister)
}

#[cfg(feature = "server")]
fn maintain_service(action: super::super::launchd_probe::MaintenanceAction) -> io::Result<()> {
    use super::super::launchd_probe::{MaintenanceEffect, probe_service};
    super::require_admin()?;
    super::super::macos_registration::maintain_registration(
        Path::new(PLIST_PATH),
        &super::validate_protected_path,
        action,
        || probe_service(LABEL),
        |effect| match effect {
            MaintenanceEffect::Enable => super::run_native("/bin/launchctl", &["enable", LABEL]),
            MaintenanceEffect::Kickstart => {
                super::run_native("/bin/launchctl", &["kickstart", LABEL])
            }
            MaintenanceEffect::Bootstrap => {
                super::run_native("/bin/launchctl", &["bootstrap", "system", PLIST_PATH])
            }
            MaintenanceEffect::Bootout => super::run_native("/bin/launchctl", &["bootout", LABEL]),
            MaintenanceEffect::RemoveRegistration => match fs::remove_file(PLIST_PATH) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_requires_matching_birth_to_remain_alive() {
        let pid = std::process::id();
        let (uid, birth) = process_identity(pid).unwrap();
        assert!(process_matches(pid, uid, birth));
        assert!(!process_matches(pid, uid, birth + 1));
    }

    #[test]
    fn authorization_script_compiles_without_requesting_authorization() {
        let output = std::env::temp_dir().join(format!(
            "zenclash-service-authorization-{}.scpt",
            std::process::id()
        ));
        let status = std::process::Command::new("/usr/bin/osacompile")
            .args(["-e", AUTHORIZATION_SCRIPT, "-o"])
            .arg(&output)
            .status()
            .unwrap();
        assert!(status.success());
        fs::remove_file(output).unwrap();
    }

    #[tokio::test]
    async fn socket_authentication_reads_real_kernel_credentials() {
        let (left, _) = UnixStream::pair().unwrap();
        let (uid, pid, birth) = authenticate_peer(&left).unwrap();
        assert_eq!(pid, std::process::id());
        assert!(process_matches(pid, uid, birth));
    }

    fn copy_script() -> &'static str {
        BOOTSTRAP_SCRIPT
            .split_once("/usr/bin/perl -e '")
            .unwrap()
            .1
            .split_once("' -- ")
            .unwrap()
            .0
    }

    fn test_directory() -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "zenclash-bootstrap-copy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        directory
    }

    #[tokio::test]
    async fn secure_bootstrap_only_copies_the_approved_regular_file_bytes() {
        let directory = test_directory();
        let source = directory.join("source");
        let output = directory.join("output");
        fs::write(&source, b"abc").unwrap();
        let status = Command::new("/usr/bin/perl")
            .args(["-e", copy_script(), "--"])
            .arg(&source)
            .arg(&output)
            .arg("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
            .status()
            .await
            .unwrap();
        assert!(status.success());
        assert_eq!(fs::read(&output).unwrap(), b"abc");
        fs::remove_file(output).unwrap();
        fs::remove_file(source).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[tokio::test]
    async fn secure_bootstrap_rejects_a_fifo_without_waiting_for_a_writer() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let directory = test_directory();
        let source = directory.join("source");
        let output = directory.join("output");
        let path = CString::new(source.as_os_str().as_bytes()).unwrap();
        // SAFETY: path is NUL terminated, test-owned and lives through mkfifo.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let mut child = Command::new("/usr/bin/perl")
            .args(["-e", copy_script(), "--"])
            .arg(&source)
            .arg(&output)
            .arg("0".repeat(64))
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let status = tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(!status.success());
        assert!(!output.exists());
        fs::remove_file(source).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[tokio::test]
    async fn secure_bootstrap_rejects_symlinks_instead_of_following_them() {
        let directory = test_directory();
        let original = directory.join("original");
        let source = directory.join("source");
        let output = directory.join("output");
        fs::write(&original, b"abc").unwrap();
        std::os::unix::fs::symlink(&original, &source).unwrap();
        let status = Command::new("/usr/bin/perl")
            .args(["-e", copy_script(), "--"])
            .arg(&source)
            .arg(&output)
            .arg("0".repeat(64))
            .stderr(Stdio::null())
            .status()
            .await
            .unwrap();
        assert!(!status.success());
        assert!(!output.exists());
        fs::remove_file(source).unwrap();
        fs::remove_file(original).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
