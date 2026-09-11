//! Local uplink observations; remote service failures are not link loss.

/// Availability of a non-tunnel network interface with a usable address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkReachability {
    /// A physical or host-provided uplink has an address and active link.
    Online,
    /// Interface enumeration succeeded but no usable uplink exists.
    Offline,
    /// The operating system could not provide a trustworthy observation.
    Unknown,
}

impl NetworkReachability {
    /// Reads local interfaces. Run this blocking operation off the UI thread.
    #[must_use]
    pub fn detect() -> Self {
        detect()
    }
}

#[cfg(unix)]
fn detect() -> NetworkReachability {
    use std::{
        ffi::CStr,
        net::{Ipv4Addr, Ipv6Addr},
    };
    let mut head = std::ptr::null_mut();
    // SAFETY: getifaddrs initializes head; the list remains alive until freeifaddrs.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return NetworkReachability::Unknown;
    }
    let mut current = head;
    let mut online = false;
    while !current.is_null() {
        // SAFETY: current is an entry in the live getifaddrs list.
        let interface = unsafe { &*current };
        if !interface.ifa_addr.is_null() && !interface.ifa_name.is_null() {
            // SAFETY: the OS provides a NUL-terminated interface name and sockaddr.
            let name = unsafe { CStr::from_ptr(interface.ifa_name) }.to_string_lossy();
            let flags = interface.ifa_flags;
            if eligible_interface(&name)
                && flags & (libc::IFF_UP as u32) != 0
                && flags & (libc::IFF_RUNNING as u32) != 0
            {
                // SAFETY: the family discriminates the sockaddr layout below.
                let family = unsafe { (*interface.ifa_addr).sa_family };
                let usable = match i32::from(family) {
                    libc::AF_INET => {
                        // SAFETY: AF_INET addresses have sockaddr_in layout.
                        let address = unsafe { &*interface.ifa_addr.cast::<libc::sockaddr_in>() };
                        let ip = Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes());
                        !ip.is_unspecified() && !ip.is_loopback() && !ip.is_link_local()
                    }
                    libc::AF_INET6 => {
                        // SAFETY: AF_INET6 addresses have sockaddr_in6 layout.
                        let address = unsafe { &*interface.ifa_addr.cast::<libc::sockaddr_in6>() };
                        let ip = Ipv6Addr::from(address.sin6_addr.s6_addr);
                        !ip.is_unspecified() && !ip.is_loopback() && !ip.is_unicast_link_local()
                    }
                    _ => false,
                };
                online |= usable;
            }
        }
        current = interface.ifa_next;
    }
    // SAFETY: head is the list returned by the successful getifaddrs call.
    unsafe { libc::freeifaddrs(head) };
    if online {
        NetworkReachability::Online
    } else {
        NetworkReachability::Offline
    }
}

#[cfg(any(unix, test))]
fn eligible_interface(name: &str) -> bool {
    ![
        "lo",
        "utun",
        "tun",
        "tap",
        "wg",
        "tailscale",
        "docker",
        "veth",
        "br-",
        "awdl",
        "llw",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

#[cfg(windows)]
fn detect() -> NetworkReachability {
    let script = "$ErrorActionPreference='Stop'; $active=@(Get-NetIPConfiguration | Where-Object {$_.NetAdapter.HardwareInterface -and $_.NetAdapter.Status -eq 'Up' -and ($_.IPv4Address -or $_.IPv6Address)}); if($active.Count -gt 0){'online'}else{'offline'}";
    match crate::platform_command::output(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", script],
    ) {
        Ok(output) if output.status.success() => {
            match String::from_utf8_lossy(&output.stdout).trim() {
                "online" => NetworkReachability::Online,
                "offline" => NetworkReachability::Offline,
                _ => NetworkReachability::Unknown,
            }
        }
        _ => NetworkReachability::Unknown,
    }
}

#[cfg(not(any(unix, windows)))]
fn detect() -> NetworkReachability {
    NetworkReachability::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_and_local_interfaces_do_not_keep_a_disconnected_host_online() {
        for name in ["lo0", "utun9", "tun0", "wg0", "docker0", "awdl0", "llw0"] {
            assert!(!eligible_interface(name));
        }
        for name in ["en0", "eth0", "wlan0", "enp3s0"] {
            assert!(eligible_interface(name));
        }
    }
}
