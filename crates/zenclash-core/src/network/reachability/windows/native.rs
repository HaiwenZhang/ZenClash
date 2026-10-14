use std::{ffi::c_void, ptr};

use windows_sys::Win32::{
    Foundation::{ERROR_NOT_FOUND, NO_ERROR},
    NetworkManagement::IpHelper::{
        FreeMibTable, GetIfTable2, GetUnicastIpAddressTable, MIB_IF_ROW2, MIB_UNICASTIPADDRESS_ROW,
    },
    Networking::WinSock::AF_UNSPEC,
};

use super::super::NetworkReachability;

pub(in crate::network::reachability) fn detect() -> NetworkReachability {
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
    super::uplink::classify(interfaces, addresses)
}

struct MibAllocation(*const c_void);

impl Drop for MibAllocation {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns a successful IP Helper table allocation.
        unsafe { FreeMibTable(self.0) };
    }
}
