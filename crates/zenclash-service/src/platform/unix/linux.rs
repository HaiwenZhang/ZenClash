use std::{fs, io, path::Path, process::Stdio, time::Duration};

use tokio::{net::UnixStream, process::Command};

#[cfg(feature = "server")]
pub(super) fn service_root() -> &'static str {
    "/var/lib/zenclash-service"
}
pub(super) fn socket_path() -> &'static str {
    "/run/zenclash-service/control.sock"
}

pub(super) fn process_identity(pid: u32) -> io::Result<(u32, u64)> {
    let root = format!("/proc/{pid}");
    let stat_before = fs::read_to_string(format!("{root}/stat"))?;
    let birth = parse_start_time(&stat_before)?;
    let status = fs::read_to_string(format!("{root}/status"))?;
    if status
        .lines()
        .find_map(|line| line.strip_prefix("State:"))
        .is_some_and(|state| state.trim_start().starts_with('Z'))
    {
        return Err(io::Error::new(io::ErrorKind::NotFound, "process ended"));
    }
    let uid = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|uids| uids.split_whitespace().nth(1))
        .and_then(|uid| uid.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "process UID unavailable"))?;
    if parse_start_time(&fs::read_to_string(format!("{root}/stat"))?)? != birth {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "process identity changed",
        ));
    }
    Ok((uid, birth))
}

fn parse_start_time(stat: &str) -> io::Result<u64> {
    // comm may contain spaces and parentheses; the final ')' precedes field 3.
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .and_then(|value| value.parse().ok())
        .filter(|birth| *birth != 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "process birth unavailable"))
}

pub(super) fn process_matches(pid: u32, uid: u32, birth: u64) -> bool {
    process_identity(pid).is_ok_and(|identity| identity == (uid, birth))
}

pub(super) fn authenticate_peer(stream: &UnixStream) -> io::Result<(u32, u32, u64)> {
    let peer = stream.peer_cred()?;
    let pid = peer
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "socket peer PID unavailable",
            )
        })?;
    let (uid, birth) = process_identity(pid)?;
    if uid != peer.uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket peer UID changed",
        ));
    }
    Ok((uid, pid, birth))
}

#[cfg(feature = "server")]
pub(super) fn prepare_socket_directory() -> io::Result<()> {
    super::create_protected_directory(Path::new("/run/zenclash-service"), 0o755)
}

pub(super) const BOOTSTRAP_SCRIPT: &str = concat!(
    "set -eu\n",
    "source=$1; expected=$2; shift 2\n",
    "umask 077\n",
    "temporary=$(/usr/bin/mktemp -d /var/lib/.zenclash-bootstrap-XXXXXXXX)\n",
    "trap '/bin/rm -f \"$temporary/helper\"; /bin/rmdir \"$temporary\"' EXIT\n",
    "[ -f \"$source\" ] && [ ! -L \"$source\" ] || exit 65\n",
    "/usr/bin/timeout --signal=KILL 15 /bin/dd if=\"$source\" of=\"$temporary/helper\" bs=65536 count=4097 2>/dev/null\n",
    "digest=$(/usr/bin/sha256sum \"$temporary/helper\")\n",
    "[ \"${digest%% *}\" = \"$expected\" ] || exit 65\n",
    "/bin/chmod 500 \"$temporary/helper\"\n",
    "\"$temporary/helper\" \"$@\"\n"
);

pub(super) async fn request_maintenance(arguments: &[std::ffi::OsString]) -> io::Result<()> {
    if !Path::new("/run/systemd/system").is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "systemd is required; see manual service installation instructions",
        ));
    }
    let mut child = Command::new("/usr/bin/pkexec")
        .arg("--disable-internal-agent")
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let status = tokio::time::timeout(Duration::from_secs(120), child.wait()).await??;
    match status.code() {
        Some(0) => Ok(()),
        Some(126) => Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "authorization cancelled",
        )),
        Some(127) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Polkit authorization unavailable or denied; a graphical authentication agent is required",
        )),
        _ => Err(io::Error::other("authorized service maintenance failed")),
    }
}

#[cfg(feature = "server")]
const UNIT_PATH: &str = "/etc/systemd/system/zenclash-service.service";
#[cfg(feature = "server")]
const UNIT: &str = include_str!("../../../../../platforms/linux/zenclash-service.service");

#[cfg(feature = "server")]
pub(super) fn register_service() -> io::Result<()> {
    super::require_admin()?;
    super::validate_protected_path(Path::new("/etc/systemd/system"), true)?;
    let temp = Path::new("/etc/systemd/system/.zenclash-service.service.new");
    super::write_registration(temp, Path::new(UNIT_PATH), UNIT.as_bytes())?;
    super::run_native("/usr/bin/systemctl", &["daemon-reload"])?;
    super::run_native(
        "/usr/bin/systemctl",
        &["enable", "zenclash-service.service"],
    )
}

#[cfg(feature = "server")]
pub(super) fn start_service() -> io::Result<()> {
    super::require_admin()?;
    super::run_native("/usr/bin/systemctl", &["start", "zenclash-service.service"])
}

#[cfg(feature = "server")]
pub(super) fn stop_service() -> io::Result<()> {
    super::require_admin()?;
    // stop is idempotent for inactive units; missing registration needs no stop.
    if !Path::new(UNIT_PATH).exists()
        && !Path::new("/usr/lib/systemd/system/zenclash-service.service").exists()
    {
        return Ok(());
    }
    super::run_native("/usr/bin/systemctl", &["stop", "zenclash-service.service"])
}

#[cfg(feature = "server")]
pub(super) fn unregister_service() -> io::Result<()> {
    stop_service()?;
    if Path::new(UNIT_PATH).exists()
        || Path::new("/usr/lib/systemd/system/zenclash-service.service").exists()
    {
        super::run_native(
            "/usr/bin/systemctl",
            &["disable", "zenclash-service.service"],
        )?;
    }
    if Path::new(UNIT_PATH).exists() {
        super::validate_protected_path(Path::new(UNIT_PATH), false)?;
        // App-owned /etc override only. Package-owned /usr/lib unit and Polkit
        // policy are removed by DEB/RPM, never by an application operation.
        fs::remove_file(UNIT_PATH)?;
        super::run_native("/usr/bin/systemctl", &["daemon-reload"])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_birth_parser_handles_spaces_and_parentheses_in_name() {
        let input = format!(
            "12 (a complicated ) process) S {} 4567 0 0",
            vec!["0"; 18].join(" ")
        );
        assert_eq!(parse_start_time(&input).unwrap(), 4567);
    }

    #[test]
    fn current_process_requires_matching_birth_to_remain_alive() {
        let pid = std::process::id();
        let (uid, birth) = process_identity(pid).unwrap();
        assert!(process_matches(pid, uid, birth));
        assert!(!process_matches(pid, uid, birth + 1));
    }

    #[tokio::test]
    async fn socket_authentication_reads_real_kernel_credentials() {
        let (left, _) = UnixStream::pair().unwrap();
        let (uid, pid, birth) = authenticate_peer(&left).unwrap();
        assert_eq!(pid, std::process::id());
        assert!(process_matches(pid, uid, birth));
    }
}
