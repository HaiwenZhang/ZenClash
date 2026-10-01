use std::{ffi::c_void, ptr};

use windows_sys::Win32::{
    Foundation::{ERROR_NOT_FOUND, NO_ERROR},
    NetworkManagement::{
        IpHelper::{
            FreeMibTable, GetIfTable2, GetUnicastIpAddressTable, MIB_IF_ROW2,
            MIB_UNICASTIPADDRESS_ROW,
        },
        Ndis::IfOperStatusUp,
    },
    Networking::WinSock::{
        AF_INET, AF_INET6, AF_UNSPEC, IpPrefixOriginRouterAdvertisement, IpPrefixOriginWellKnown,
        IpSuffixOriginLinkLayerAddress, IpSuffixOriginRandom,
    },
};

use super::NetworkReachability;

pub(super) fn detect() -> NetworkReachability {
    let mut interfaces = ptr::null_mut();
    // SAFETY: the API initializes the output pointer and allocates the complete table.
    if unsafe { GetIfTable2(&mut interfaces) } != NO_ERROR || interfaces.is_null() {
        return NetworkReachability::Unknown;
    }
    let _interfaces_memory = MibAllocation(interfaces.cast());
    let mut addresses = ptr::null_mut();
    // SAFETY: AF_UNSPEC requests both families; the API initializes the output pointer.
    let result = unsafe { GetUnicastIpAddressTable(AF_UNSPEC, &mut addresses) };
    if result == ERROR_NOT_FOUND {
        return NetworkReachability::Offline;
    }
    if result != NO_ERROR || addresses.is_null() {
        return NetworkReachability::Unknown;
    }
    let _addresses_memory = MibAllocation(addresses.cast());
    // SAFETY: both successful API calls returned tables containing NumEntries rows.
    // The bindings account for the header/row alignment; guards keep both allocations live.
    let (interfaces, addresses) = unsafe {
        (
            std::slice::from_raw_parts(
                ptr::addr_of!((*interfaces).Table).cast::<MIB_IF_ROW2>(),
                (*interfaces).NumEntries as usize,
            ),
            std::slice::from_raw_parts(
                ptr::addr_of!((*addresses).Table).cast::<MIB_UNICASTIPADDRESS_ROW>(),
                (*addresses).NumEntries as usize,
            ),
        )
    };
    classify(interfaces, addresses)
}

struct MibAllocation(*const c_void);

impl Drop for MibAllocation {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns a successful IP Helper table allocation.
        unsafe { FreeMibTable(self.0) };
    }
}

fn classify(
    interfaces: &[MIB_IF_ROW2],
    addresses: &[MIB_UNICASTIPADDRESS_ROW],
) -> NetworkReachability {
    // HardwareInterface is bit 0 in the Windows SDK's InterfaceAndOperStatusFlags.
    let online = interfaces.iter().any(|interface| {
        interface.InterfaceAndOperStatusFlags._bitfield & 1 != 0
            && interface.OperStatus == IfOperStatusUp
            && addresses.iter().any(|address| {
                address.InterfaceIndex == interface.InterfaceIndex && usable_address(address)
            })
    });
    if online {
        NetworkReachability::Online
    } else {
        NetworkReachability::Offline
    }
}

fn usable_address(address: &MIB_UNICASTIPADDRESS_ROW) -> bool {
    // SAFETY: the family discriminates the sockaddr union, whose integer/byte fields
    // have no invalid bit patterns. Match Get-NetIPConfiguration's IPv4Address/IPv6Address;
    // that cmdlet puts temporary and link-local IPv6 addresses into separate properties.
    unsafe {
        match address.Address.si_family {
            AF_INET => true,
            AF_INET6 => {
                let temporary = address.PrefixOrigin == IpPrefixOriginRouterAdvertisement
                    && address.SuffixOrigin == IpSuffixOriginRandom;
                let link_local = address.PrefixOrigin == IpPrefixOriginWellKnown
                    && address.SuffixOrigin == IpSuffixOriginLinkLayerAddress;
                !temporary && !link_local
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::NetworkManagement::Ndis::{IfOperStatusDown, IfOperStatusUp};

    fn interface(index: u32, physical: bool, up: bool) -> MIB_IF_ROW2 {
        // SAFETY: all fields in this Windows POD structure permit zero values.
        let mut row: MIB_IF_ROW2 = unsafe { std::mem::zeroed() };
        row.InterfaceIndex = index;
        row.InterfaceAndOperStatusFlags._bitfield = u8::from(physical);
        row.OperStatus = if up { IfOperStatusUp } else { IfOperStatusDown };
        row
    }

    fn address(index: u32, ip: std::net::IpAddr) -> MIB_UNICASTIPADDRESS_ROW {
        // SAFETY: all fields in this Windows POD structure permit zero values.
        let mut row: MIB_UNICASTIPADDRESS_ROW = unsafe { std::mem::zeroed() };
        row.InterfaceIndex = index;
        match ip {
            std::net::IpAddr::V4(ip) => {
                row.Address.Ipv4.sin_family = AF_INET;
                row.Address.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(ip.octets());
            }
            std::net::IpAddr::V6(ip) => {
                row.Address.Ipv6.sin6_family = AF_INET6;
                row.Address.Ipv6.sin6_addr.u.Byte = ip.octets();
                if ip.is_unicast_link_local() {
                    row.PrefixOrigin = IpPrefixOriginWellKnown;
                    row.SuffixOrigin = IpSuffixOriginLinkLayerAddress;
                }
            }
        }
        row
    }

    #[test]
    fn active_physical_uplink_accepts_ipv4_and_routable_ipv6() {
        for ip in ["192.0.2.1", "2001:db8::1"] {
            assert_eq!(
                classify(
                    &[interface(3, true, true)],
                    &[address(3, ip.parse().unwrap())]
                ),
                NetworkReachability::Online
            );
        }
    }

    #[test]
    fn virtual_or_down_interfaces_do_not_keep_a_disconnected_host_online() {
        let addresses = [address(3, "192.0.2.1".parse().unwrap())];
        for candidate in [interface(3, false, true), interface(3, true, false)] {
            assert_eq!(
                classify(&[candidate], &addresses),
                NetworkReachability::Offline
            );
        }
    }

    #[test]
    fn uplink_needs_an_address_on_the_same_interface() {
        let interfaces = [interface(3, true, true)];
        assert_eq!(classify(&interfaces, &[]), NetworkReachability::Offline);
        assert_eq!(
            classify(&interfaces, &[address(4, "192.0.2.1".parse().unwrap())]),
            NetworkReachability::Offline
        );
    }

    #[test]
    fn ipv6_link_local_alone_is_not_a_routable_uplink() {
        assert_eq!(
            classify(
                &[interface(3, true, true)],
                &[address(3, "fe80::1".parse().unwrap())]
            ),
            NetworkReachability::Offline
        );
    }

    #[test]
    fn temporary_ipv6_alone_preserves_the_previous_observation() {
        let mut temporary = address(3, "2001:db8::1".parse().unwrap());
        temporary.PrefixOrigin = IpPrefixOriginRouterAdvertisement;
        temporary.SuffixOrigin = IpSuffixOriginRandom;
        assert_eq!(
            classify(&[interface(3, true, true)], &[temporary]),
            NetworkReachability::Offline
        );
    }
}
