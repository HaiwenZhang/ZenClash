#![deny(clippy::undocumented_unsafe_blocks)]

use std::{
    ffi::c_void,
    ptr::{self, NonNull},
};

use super::super::{
    SystemProxyStatus,
    native::{
        NativeProxyChange, NativeProxySnapshot, NativeProxyTarget, NativeWriteAttempt,
        invalid_snapshot, ownership_changed, verification_failure,
    },
};
use crate::{MihomoError, MihomoResult};

mod networksetup;
pub(in crate::system_proxy) use networksetup::{apply, restore};

type CfRef = *const c_void;

// ABI definitions follow the shipped CoreFoundation and SystemConfiguration
// SDK headers. Boolean is UInt8; CFNumberType and CFIndex are signed long.
#[repr(C)]
struct ValueCallbacks {
    version: isize,
    retain: Option<unsafe extern "C" fn(CfRef, CfRef) -> CfRef>,
    release: Option<unsafe extern "C" fn(CfRef, CfRef)>,
    copy_description: Option<unsafe extern "C" fn(CfRef) -> CfRef>,
    equal: Option<unsafe extern "C" fn(CfRef, CfRef) -> u8>,
}

#[repr(C)]
struct KeyCallbacks {
    values: ValueCallbacks,
    hash: Option<unsafe extern "C" fn(CfRef) -> usize>,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeDictionaryKeyCallBacks: KeyCallbacks;
    static kCFTypeDictionaryValueCallBacks: ValueCallbacks;
    static kCFTypeArrayCallBacks: ValueCallbacks;
    fn CFRelease(value: CfRef);
    fn CFEqual(left: CfRef, right: CfRef) -> u8;
    fn CFGetTypeID(value: CfRef) -> usize;
    fn CFErrorGetDomain(error: CfRef) -> CfRef;
    fn CFErrorGetCode(error: CfRef) -> isize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(value: CfRef) -> u8;
    fn CFArrayGetTypeID() -> usize;
    fn CFStringGetLength(value: CfRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(value: CfRef, buffer: *mut i8, capacity: isize, encoding: u32) -> u8;
    fn CFDataCreate(allocator: CfRef, bytes: *const u8, count: isize) -> CfRef;
    fn CFDataGetLength(data: CfRef) -> isize;
    fn CFDataGetBytePtr(data: CfRef) -> *const u8;
    fn CFPropertyListCreateData(
        allocator: CfRef,
        value: CfRef,
        format: isize,
        options: usize,
        error: *mut CfRef,
    ) -> CfRef;
    fn CFPropertyListCreateWithData(
        allocator: CfRef,
        data: CfRef,
        options: usize,
        format: *mut isize,
        error: *mut CfRef,
    ) -> CfRef;
    fn CFStringCreateWithBytes(
        allocator: CfRef,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        external: u8,
    ) -> CfRef;
    fn CFStringCompare(left: CfRef, right: CfRef, options: usize) -> isize;
    fn CFNumberIsFloatType(value: CfRef) -> u8;
    fn CFDictionaryGetCount(dictionary: CfRef) -> isize;
    fn CFDictionaryGetKeysAndValues(dictionary: CfRef, keys: *mut CfRef, values: *mut CfRef);
    fn CFNumberCreate(allocator: CfRef, kind: isize, value: CfRef) -> CfRef;
    fn CFArrayCreate(
        allocator: CfRef,
        values: *const CfRef,
        count: isize,
        callbacks: *const ValueCallbacks,
    ) -> CfRef;
    fn CFArrayGetCount(array: CfRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CfRef, index: isize) -> CfRef;
    fn CFDictionaryCreateMutable(
        allocator: CfRef,
        capacity: isize,
        keys: *const KeyCallbacks,
        values: *const ValueCallbacks,
    ) -> *mut c_void;
    fn CFDictionaryCreateMutableCopy(
        allocator: CfRef,
        capacity: isize,
        dictionary: CfRef,
    ) -> *mut c_void;
    fn CFDictionarySetValue(dictionary: *mut c_void, key: CfRef, value: CfRef);
    fn CFDictionaryGetValue(dictionary: CfRef, key: CfRef) -> CfRef;
    fn CFNumberGetValue(number: CfRef, kind: isize, output: *mut c_void) -> u8;
}

#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    static kSCNetworkProtocolTypeProxies: CfRef;
    fn SCError() -> i32;
    fn SCPreferencesCreate(allocator: CfRef, name: CfRef, identifier: CfRef) -> CfRef;
    fn SCNetworkServiceCopyAll(preferences: CfRef) -> CfRef;
    fn SCNetworkServiceGetName(service: CfRef) -> CfRef;
    fn SCNetworkServiceGetServiceID(service: CfRef) -> CfRef;
    fn SCNetworkServiceCopyProtocol(service: CfRef, kind: CfRef) -> CfRef;
    fn SCNetworkProtocolGetConfiguration(protocol: CfRef) -> CfRef;
}

struct OwnedCf(NonNull<c_void>);

impl OwnedCf {
    // SAFETY: A non-null pointer must be a valid +1 Create/Copy result. This
    // function consumes that reference; borrowed Get results must not enter it.
    unsafe fn created(pointer: CfRef, operation: &'static str) -> MihomoResult<Self> {
        NonNull::new(pointer.cast_mut()).map(Self).ok_or_else(|| {
            if operation.starts_with("CF") {
                crate::SystemProxyError::NativeOperationRejected {
                    backend: crate::SystemProxyBackendKind::MacOs,
                    operation,
                }
                .into()
            } else {
                native_error(operation)
            }
        })
    }

    fn string(value: &str) -> MihomoResult<Self> {
        // SAFETY: Rust UTF-8 storage is valid for this synchronous copying call;
        // 0x08000100 is kCFStringEncodingUTF8, not a C-string/NUL contract.
        unsafe {
            Self::created(
                CFStringCreateWithBytes(
                    ptr::null(),
                    value.as_ptr(),
                    value.len() as isize,
                    0x0800_0100,
                    0,
                ),
                "CFStringCreateWithBytes",
            )
        }
    }

    fn number(value: i32) -> MihomoResult<Self> {
        // SAFETY: kCFNumberSInt32Type (3) copies one valid i32 from this stack.
        unsafe {
            Self::created(
                CFNumberCreate(ptr::null(), 3, (&raw const value).cast()),
                "CFNumberCreate",
            )
        }
    }

    fn pointer(&self) -> CfRef {
        self.0.as_ptr()
    }
}

impl Drop for OwnedCf {
    fn drop(&mut self) {
        // SAFETY: Only non-null Create/Copy results enter OwnedCf, each at +1.
        unsafe { CFRelease(self.pointer()) };
    }
}

fn native_error(operation: &'static str) -> MihomoError {
    // SAFETY: SCError has no arguments or borrowed object lifetime.
    operation_error(operation, unsafe { SCError() })
}

fn operation_error(operation: &'static str, code: i32) -> MihomoError {
    crate::SystemProxyError::Platform {
        backend: crate::SystemProxyBackendKind::MacOs,
        operation,
        code: i64::from(code),
    }
    .into()
}

fn unique_service_index(matches: impl Iterator<Item = bool>) -> MihomoResult<usize> {
    let mut indices = matches
        .enumerate()
        .filter_map(|(index, matched)| matched.then_some(index));
    match (indices.next(), indices.next()) {
        (Some(index), None) => Ok(index),
        _ => Err(crate::SystemProxyError::BackendUnavailable {
            backend: crate::SystemProxyBackendKind::MacOs,
            reason: crate::SystemProxyBackendUnavailable::AmbiguousService,
        }
        .into()),
    }
}

const MAX_NATIVE_CONFIGURATION_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub(in crate::system_proxy) struct ProxySnapshot {
    service_id: String,
    summary: SystemProxyStatus,
    configuration: Option<std::sync::Arc<[u8]>>,
}

impl ProxySnapshot {
    pub(in crate::system_proxy) fn same_target(&self, other: &Self) -> bool {
        self.service_id == other.service_id
    }
    pub(in crate::system_proxy) fn legacy_release_is_safe(&self) -> bool {
        true
    }
    pub(in crate::system_proxy) fn summary(&self) -> &SystemProxyStatus {
        &self.summary
    }

    pub(in crate::system_proxy) fn same_state(&self, other: &Self) -> MihomoResult<bool> {
        if self.service_id != other.service_id {
            return Ok(false);
        }
        let left = networksetup::comparable_configuration(self)?;
        let right = networksetup::comparable_configuration(other)?;
        // SAFETY: Both owners contain live property-list dictionaries.
        unsafe { property_types_equal(left.pointer(), right.pointer(), 0) }
    }
}

struct PreferenceSession {
    preferences: OwnedCf,
}

impl PreferenceSession {
    fn open() -> MihomoResult<Self> {
        let name = OwnedCf::string("ZenClash proxy observation")?;
        // SAFETY: Null ID/allocator select system network preferences. This
        // session only reads; writes use networksetup without an app auth session.
        let pointer = unsafe { SCPreferencesCreate(ptr::null(), name.pointer(), ptr::null()) };
        // SAFETY: Create returns a +1 object or null; `OwnedCf::created` rejects
        // null and takes ownership of the successful result.
        let preferences = unsafe { OwnedCf::created(pointer, "SCPreferencesCreate") }?;
        Ok(Self { preferences })
    }

    fn select(&self, expected: &str, by_id: bool) -> MihomoResult<SelectedProtocol> {
        // SAFETY: Copy returns a +1 array or null; `OwnedCf::created` rejects
        // null and takes ownership of the successful result.
        let services = unsafe {
            OwnedCf::created(
                SCNetworkServiceCopyAll(self.preferences.pointer()),
                "SCNetworkServiceCopyAll",
            )
        }?;
        let expected = OwnedCf::string(expected)?;
        // SAFETY: The retained array contains network services. Get results remain
        // borrowed until the array is released; every index is bounded by count.
        let count = unsafe { CFArrayGetCount(services.pointer()) };
        let index = unique_service_index((0..count).map(|index| {
            // SAFETY: `index` is below the live array count; every Get result is
            // borrowed from `services` and compared before that array is released.
            unsafe {
                let service = CFArrayGetValueAtIndex(services.pointer(), index);
                let identity = if by_id {
                    SCNetworkServiceGetServiceID(service)
                } else {
                    SCNetworkServiceGetName(service)
                };
                !identity.is_null() && CFStringCompare(identity, expected.pointer(), 0) == 0
            }
        }))?;
        // SAFETY: The selected index was bounded and unique. Copy retains its
        // protocol independently of the services array; Get strings are copied.
        unsafe {
            let service = CFArrayGetValueAtIndex(services.pointer(), index as isize);
            Ok(SelectedProtocol {
                id: read_cf_string(SCNetworkServiceGetServiceID(service))?,
                name: read_cf_string(SCNetworkServiceGetName(service))?,
                protocol: OwnedCf::created(
                    SCNetworkServiceCopyProtocol(service, kSCNetworkProtocolTypeProxies),
                    "SCNetworkServiceCopyProtocol",
                )?,
            })
        }
    }
}

struct SelectedProtocol {
    id: String,
    name: String,
    protocol: OwnedCf,
}

impl SelectedProtocol {
    fn capture(&self) -> MihomoResult<ProxySnapshot> {
        // SAFETY: The configuration is borrowed from this live protocol and is
        // serialized synchronously before that owner can be released.
        let configuration = unsafe {
            encode_configuration(SCNetworkProtocolGetConfiguration(self.protocol.pointer()))
        }?;
        let summary = summary_from_configuration(&self.name, configuration.as_deref())?;
        Ok(ProxySnapshot {
            service_id: self.id.clone(),
            summary,
            configuration,
        })
    }
}

pub(in crate::system_proxy) fn capture(
    target: NativeProxyTarget<'_>,
) -> MihomoResult<NativeProxySnapshot> {
    let session = PreferenceSession::open()?;
    let selected = match target {
        NativeProxyTarget::Service(name) => session.select(name, false)?,
        NativeProxyTarget::Captured(NativeProxySnapshot::Platform(snapshot)) => {
            session.select(&snapshot.service_id, true)?
        }
        #[cfg(test)]
        NativeProxyTarget::Captured(_) => return Err(ownership_changed()),
    };
    Ok(NativeProxySnapshot::Platform(selected.capture()?))
}

impl ProxySnapshot {
    fn with_change(&self, change: &NativeProxyChange<'_>) -> MihomoResult<Self> {
        let dictionary = editable_configuration(self.configuration.as_deref())?;
        match change {
            NativeProxyChange::Disable => set_flags(&dictionary, false, false, false)?,
            NativeProxyChange::Pac { url } => {
                set_flags(&dictionary, false, false, true)?;
                set_string(&dictionary, "ProxyAutoConfigURLString", url)?;
            }
            NativeProxyChange::Manual { host, port, bypass } => {
                set_flags(&dictionary, true, true, false)?;
                set_string(&dictionary, "HTTPProxy", host)?;
                set_string(&dictionary, "HTTPSProxy", host)?;
                set_number(&dictionary, "HTTPPort", i32::from(*port))?;
                set_number(&dictionary, "HTTPSPort", i32::from(*port))?;
                set_bypass(&dictionary, bypass)?;
            }
        }
        // SAFETY: This owned dictionary has retaining CFType callbacks.
        let configuration = unsafe { encode_configuration(dictionary.pointer()) }?;
        Ok(ProxySnapshot {
            service_id: self.service_id.clone(),
            summary: summary_from_configuration(&self.summary.service, configuration.as_deref())?,
            configuration,
        })
    }
}

// SAFETY: The dictionary is null or borrowed from a live owned protocol/CF object.
unsafe fn encode_configuration(dictionary: CfRef) -> MihomoResult<Option<std::sync::Arc<[u8]>>> {
    if dictionary.is_null() {
        return Ok(None);
    }
    // SAFETY: GetTypeID accepts this live CF value; proxy configs must be dictionaries.
    if unsafe { CFGetTypeID(dictionary) != CFDictionaryGetTypeID() } {
        return Err(invalid_snapshot());
    }
    let mut error = ptr::null();
    // SAFETY: Binary property-list v1.0 is format 200; the output error owns +1.
    let data = unsafe { CFPropertyListCreateData(ptr::null(), dictionary, 200, 0, &raw mut error) };
    // SAFETY: The optional error is a +1 CFError from this call.
    let cause = unsafe { property_list_error(error, "CFPropertyListCreateData") };
    if data.is_null() {
        return Err(crate::SystemProxyError::SnapshotEncodingFailed { cause }.into());
    }
    // SAFETY: Create returned an owned data object or null, now consumed by RAII.
    let data = unsafe { OwnedCf::created(data, "CFPropertyListCreateData") }?;
    // SAFETY: This is a live CFData; its byte buffer is borrowed for the copy.
    let length = usize::try_from(unsafe { CFDataGetLength(data.pointer()) })
        .map_err(|_| invalid_snapshot())?;
    if length == 0 || length > MAX_NATIVE_CONFIGURATION_BYTES {
        return Err(invalid_snapshot());
    }
    // SAFETY: `data` owns a live CFData; its buffer remains valid until the
    // owner is dropped at the end of this function.
    let bytes = unsafe { CFDataGetBytePtr(data.pointer()) };
    if bytes.is_null() {
        return Err(invalid_snapshot());
    }
    Ok(Some(
        // SAFETY: `bytes` is non-null and `length` was bounded against this live
        // CFData's byte length; the slice is copied immediately.
        unsafe { std::slice::from_raw_parts(bytes, length) }.into(),
    ))
}

fn decode_configuration(bytes: Option<&[u8]>) -> MihomoResult<Option<OwnedCf>> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    if bytes.is_empty() || bytes.len() > MAX_NATIVE_CONFIGURATION_BYTES {
        return Err(invalid_snapshot());
    }
    // SAFETY: The synchronous Create copies this bounded live byte slice.
    let data = unsafe {
        OwnedCf::created(
            CFDataCreate(ptr::null(), bytes.as_ptr(), bytes.len() as isize),
            "CFDataCreate",
        )
    }?;
    let mut error = ptr::null();
    // SAFETY: Immutable decode of valid owned CFData, with optional format output.
    let value = unsafe {
        CFPropertyListCreateWithData(
            ptr::null(),
            data.pointer(),
            0,
            ptr::null_mut(),
            &raw mut error,
        )
    };
    // SAFETY: The optional error is a +1 CFError from this call.
    let cause = unsafe { property_list_error(error, "CFPropertyListCreateWithData") };
    if value.is_null() {
        return Err(crate::SystemProxyError::InvalidNativeSnapshot { cause }.into());
    }
    // SAFETY: Decode's +1 result is consumed exactly once by this owner.
    let value = unsafe { OwnedCf::created(value, "CFPropertyListCreateWithData") }?;
    // SAFETY: Type checking precedes every dictionary operation.
    if unsafe { CFGetTypeID(value.pointer()) != CFDictionaryGetTypeID() } {
        return Err(invalid_snapshot());
    }
    Ok(Some(value))
}

// SAFETY: error is null or a +1 CFError returned by a property-list Create call.
unsafe fn property_list_error(
    error: CfRef,
    operation: &'static str,
) -> Option<crate::NativePropertyListError> {
    let error = NonNull::new(error.cast_mut()).map(OwnedCf)?;
    // SAFETY: The owned CFError remains live while its borrowed domain and code are copied.
    let domain = unsafe { read_cf_string(CFErrorGetDomain(error.pointer())) }.ok();
    // SAFETY: `error` is an owned live CFError for the rest of this function.
    let code = unsafe { CFErrorGetCode(error.pointer()) } as i64;
    Some(crate::NativePropertyListError::new(operation, domain, code))
}

#[cfg(test)]
fn configurations_equal(left: Option<&[u8]>, right: Option<&[u8]>) -> MihomoResult<bool> {
    match (decode_configuration(left)?, decode_configuration(right)?) {
        (None, None) => Ok(true),
        (Some(left), Some(right)) => {
            // SAFETY: Both are live property lists decoded as dictionaries.
            unsafe { property_types_equal(left.pointer(), right.pointer(), 0) }
        }
        _ => Ok(false),
    }
}

// SAFETY: Both references are valid property-list objects, live throughout recursion.
unsafe fn property_types_equal(left: CfRef, right: CfRef, depth: usize) -> MihomoResult<bool> {
    if depth > 64 {
        return Err(invalid_snapshot());
    }
    // SAFETY: Both references are valid property-list objects for this call.
    let left_kind = unsafe { CFGetTypeID(left) };
    // SAFETY: Both references are valid property-list objects for this call.
    let right_kind = unsafe { CFGetTypeID(right) };
    // SAFETY: Both references are valid property-list objects for this call.
    let equal = unsafe { CFEqual(left, right) };
    if left_kind != right_kind || equal == 0 {
        return Ok(false);
    }
    // SAFETY: `left` is a live CFNumber when the type ID matches.
    if left_kind == unsafe { CFNumberGetTypeID() } {
        // SAFETY: Both operands are live CFNumber instances.
        return Ok(unsafe { CFNumberIsFloatType(left) == CFNumberIsFloatType(right) });
    }
    // SAFETY: `left` is a live CFArray when the type ID matches.
    if left_kind == unsafe { CFArrayGetTypeID() } {
        // SAFETY: Both references are live CFArrays.
        let count = unsafe { CFArrayGetCount(left) };
        for index in 0..count {
            // SAFETY: `index` is bounded by the live array count; both array
            // objects remain live throughout this call.
            let left_value = unsafe { CFArrayGetValueAtIndex(left, index) };
            // SAFETY: Same index and count contract as the left array.
            let right_value = unsafe { CFArrayGetValueAtIndex(right, index) };
            // SAFETY: Both values are borrowed live property-list objects.
            if !unsafe { property_types_equal(left_value, right_value, depth + 1) }? {
                return Ok(false);
            }
        }
    // SAFETY: `left` is a live CFDictionary when the type ID matches.
    } else if left_kind == unsafe { CFDictionaryGetTypeID() } {
        // SAFETY: `left` is a live CFDictionary; the count only bounds the
        // arrays written below.
        let count = usize::try_from(unsafe { CFDictionaryGetCount(left) })
            .map_err(|_| invalid_snapshot())?;
        if count > MAX_NATIVE_CONFIGURATION_BYTES {
            return Err(invalid_snapshot());
        }
        let mut keys = vec![ptr::null(); count];
        let mut values = vec![ptr::null(); count];
        // SAFETY: Arrays each have capacity for the full dictionary entry count.
        unsafe { CFDictionaryGetKeysAndValues(left, keys.as_mut_ptr(), values.as_mut_ptr()) };
        for (key, value) in keys.into_iter().zip(values) {
            // SAFETY: `right` is a live dictionary and `key` comes from `left`;
            // a missing key returns null, which is handled below.
            let other = unsafe { CFDictionaryGetValue(right, key) };
            if other.is_null() {
                return Ok(false);
            }
            // SAFETY: Both values are borrowed live property-list objects from
            // their respective dictionaries.
            if !unsafe { property_types_equal(value, other, depth + 1) }? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn editable_configuration(bytes: Option<&[u8]>) -> MihomoResult<OwnedCf> {
    let original = decode_configuration(bytes)?;
    // SAFETY: An owned dictionary is copied; absent config uses retaining callbacks.
    unsafe {
        OwnedCf::created(
            if let Some(original) = original {
                CFDictionaryCreateMutableCopy(ptr::null(), 0, original.pointer())
            } else {
                CFDictionaryCreateMutable(
                    ptr::null(),
                    0,
                    &raw const kCFTypeDictionaryKeyCallBacks,
                    &raw const kCFTypeDictionaryValueCallBacks,
                )
            },
            "CFDictionaryCreateMutableCopy",
        )
    }
}

fn set_string(dictionary: &OwnedCf, name: &str, value: &str) -> MihomoResult<()> {
    let key = OwnedCf::string(name)?;
    let value = OwnedCf::string(value)?;
    // SAFETY: Editable config is mutable and retains these live CFType objects.
    unsafe { CFDictionarySetValue(dictionary.0.as_ptr(), key.pointer(), value.pointer()) };
    Ok(())
}
fn set_number(dictionary: &OwnedCf, name: &str, value: i32) -> MihomoResult<()> {
    let key = OwnedCf::string(name)?;
    let value = OwnedCf::number(value)?;
    // SAFETY: Editable config is mutable and retains this live CFNumber under
    // the live CFString key.
    unsafe { CFDictionarySetValue(dictionary.0.as_ptr(), key.pointer(), value.pointer()) };
    Ok(())
}
fn set_flags(dictionary: &OwnedCf, http: bool, https: bool, pac: bool) -> MihomoResult<()> {
    for (key, value) in [
        ("HTTPEnable", http),
        ("HTTPSEnable", https),
        ("ProxyAutoConfigEnable", pac),
    ] {
        set_number(dictionary, key, i32::from(value))?;
    }
    Ok(())
}
fn set_bypass(dictionary: &OwnedCf, entries: &[String]) -> MihomoResult<()> {
    let entries = entries
        .iter()
        .map(|entry| OwnedCf::string(entry))
        .collect::<MihomoResult<Vec<_>>>()?;
    let pointers = entries.iter().map(OwnedCf::pointer).collect::<Vec<_>>();
    // SAFETY: Retaining callbacks keep every live string; an empty array is valid.
    let array = unsafe {
        OwnedCf::created(
            CFArrayCreate(
                ptr::null(),
                pointers.as_ptr(),
                pointers.len() as isize,
                &raw const kCFTypeArrayCallBacks,
            ),
            "CFArrayCreate",
        )
    }?;
    let key = OwnedCf::string("ExceptionsList")?;
    // SAFETY: Editable config is mutable and retains this live CFArray under the
    // live CFString key.
    unsafe { CFDictionarySetValue(dictionary.0.as_ptr(), key.pointer(), array.pointer()) };
    Ok(())
}

// SAFETY: The value is a live CFString or null; null is never converted as a string.
unsafe fn read_cf_string(value: CfRef) -> MihomoResult<String> {
    // SAFETY: `value` is checked non-null; the caller guarantees a live CF object.
    if value.is_null() || unsafe { CFGetTypeID(value) != CFStringGetTypeID() } {
        return Err(invalid_snapshot());
    }
    // SAFETY: `value` is a live CFString; the encoding constant is the same UTF-8
    // contract used by the copy below.
    let capacity =
        unsafe { CFStringGetMaximumSizeForEncoding(CFStringGetLength(value), 0x0800_0100) };
    let capacity = usize::try_from(capacity)
        .map_err(|_| invalid_snapshot())?
        .checked_add(1)
        .filter(|capacity| *capacity <= MAX_NATIVE_CONFIGURATION_BYTES)
        .ok_or_else(invalid_snapshot)?;
    let mut buffer = vec![0_u8; capacity];
    // SAFETY: `buffer` has exactly the capacity passed to the API and the live
    // CFString is copied into it synchronously.
    if unsafe {
        CFStringGetCString(
            value,
            buffer.as_mut_ptr().cast(),
            capacity as isize,
            0x0800_0100,
        )
    } == 0
    {
        return Err(invalid_snapshot());
    }
    std::ffi::CStr::from_bytes_until_nul(&buffer)
        .map_err(|_| invalid_snapshot())?
        .to_str()
        .map(str::to_owned)
        .map_err(|_| invalid_snapshot())
}

fn summary_from_configuration(
    service: &str,
    bytes: Option<&[u8]>,
) -> MihomoResult<SystemProxyStatus> {
    let dictionary = decode_configuration(bytes)?;
    let Some(dictionary) = dictionary else {
        return Ok(SystemProxyStatus {
            service: service.into(),
            ..Default::default()
        });
    };
    let value = |name: &str| -> MihomoResult<CfRef> {
        let key = OwnedCf::string(name)?;
        // SAFETY: `dictionary` owns a live immutable dictionary; the returned
        // value is borrowed and only used while that dictionary is alive.
        Ok(unsafe { CFDictionaryGetValue(dictionary.pointer(), key.pointer()) })
    };
    let number = |name: &str| -> MihomoResult<i64> {
        let value = value(name)?;
        if value.is_null() {
            return Ok(0);
        }
        // SAFETY: `value` is a borrowed live CFType from the owned dictionary.
        let kind = unsafe { CFGetTypeID(value) };
        // SAFETY: `kind` was read from this same live object.
        if kind == unsafe { CFBooleanGetTypeID() } {
            // SAFETY: `value` is a live CFBoolean.
            return Ok(i64::from(unsafe { CFBooleanGetValue(value) }));
        }
        // SAFETY: `kind` was read from this same live object.
        if kind != unsafe { CFNumberGetTypeID() } {
            return Err(invalid_snapshot());
        }
        let mut number = 0_i64;
        // SAFETY: Type checked CFNumber; kCFNumberSInt64Type (4) copies into i64.
        if unsafe { CFNumberGetValue(value, 4, (&raw mut number).cast()) } == 0 {
            return Err(invalid_snapshot());
        }
        Ok(number)
    };
    let string = |name: &str| -> MihomoResult<String> {
        let value = value(name)?;
        if value.is_null() {
            Ok(String::new())
        } else {
            // SAFETY: `value` is a borrowed live CFString from the owned dictionary.
            unsafe { read_cf_string(value) }
        }
    };
    let bypass = value("ExceptionsList")?;
    let bypass = if bypass.is_null() {
        Vec::new()
    } else {
        // SAFETY: `bypass` is a borrowed live CFArray from the owned dictionary.
        if unsafe { CFGetTypeID(bypass) != CFArrayGetTypeID() } {
            return Err(invalid_snapshot());
        }
        // SAFETY: `bypass` is a live CFArray for the rest of this block.
        let bypass_count = unsafe { CFArrayGetCount(bypass) };
        (0..bypass_count)
            .map(|index| {
                // SAFETY: `index` is bounded by the live array count; the element
                // is borrowed while the array remains alive.
                unsafe { read_cf_string(CFArrayGetValueAtIndex(bypass, index)) }
            })
            .collect::<MihomoResult<Vec<_>>>()?
    };
    Ok(SystemProxyStatus {
        service: service.into(),
        enabled: number("HTTPEnable")? != 0,
        server: string("HTTPProxy")?,
        port: u16::try_from(number("HTTPPort")?).map_err(|_| invalid_snapshot())?,
        secure_enabled: number("HTTPSEnable")? != 0,
        secure_server: string("HTTPSProxy")?,
        secure_port: u16::try_from(number("HTTPSPort")?).map_err(|_| invalid_snapshot())?,
        bypass,
        auto_enabled: number("ProxyAutoConfigEnable")? != 0,
        auto_url: string("ProxyAutoConfigURLString")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_plist_retains_the_native_error_code_without_input_contents() {
        use std::error::Error;
        let error = match decode_configuration(Some(b"not a property list: private-password")) {
            Err(error) => error,
            Ok(_) => panic!("malformed property list must be rejected"),
        };
        assert!(matches!(&error, MihomoError::SystemProxy(error)
            if matches!(error.as_ref(), crate::SystemProxyError::InvalidNativeSnapshot { cause: Some(cause) }
                if cause.operation() == "CFPropertyListCreateWithData"
                    && cause.code() != 0 && cause.domain().is_some())));
        assert!(
            error
                .source()
                .unwrap()
                .is::<crate::NativePropertyListError>()
        );
        assert!(!format!("{error:?} {error}").contains("private-password"));
    }

    #[test]
    fn allocation_and_native_errors_remain_errors_without_native_writes() {
        // SAFETY: Null is an allowed failed Create result and owns no object.
        assert!(unsafe { OwnedCf::created(ptr::null(), "fixture allocation") }.is_err());
        assert!(
            operation_error("SCPreferencesCreate", 1001)
                .to_string()
                .contains("1001")
        );
    }

    #[test]
    fn refuses_missing_or_ambiguous_service_names_before_any_write() {
        assert!(unique_service_index([false, false].into_iter()).is_err());
        assert!(unique_service_index([true, true].into_iter()).is_err());
        assert_eq!(
            unique_service_index([false, true, false].into_iter()).unwrap(),
            1
        );
    }

    fn number(dictionary: &OwnedCf, name: &str) -> i32 {
        let key = OwnedCf::string(name).unwrap();
        let mut value = -1_i32;
        // SAFETY: Mapping produces CFNumber values under these keys; both the
        // borrowed value and i32 output buffer remain valid through the call.
        unsafe {
            let number = CFDictionaryGetValue(dictionary.pointer(), key.pointer());
            assert!(!number.is_null());
            assert_ne!(CFNumberGetValue(number, 3, (&raw mut value).cast()), 0);
        }
        value
    }

    fn string_equals(dictionary: &OwnedCf, name: &str, expected: &str) -> bool {
        let key = OwnedCf::string(name).unwrap();
        let expected = OwnedCf::string(expected).unwrap();
        // SAFETY: Mapping produces retained strings under these keys.
        unsafe {
            let value = CFDictionaryGetValue(dictionary.pointer(), key.pointer());
            !value.is_null() && CFStringCompare(value, expected.pointer(), 0) == 0
        }
    }
    fn fixture_snapshot(dictionary: &OwnedCf) -> ProxySnapshot {
        // SAFETY: Test fixture dictionary is a live owned CFDictionary.
        let configuration = unsafe { encode_configuration(dictionary.pointer()) }.unwrap();
        ProxySnapshot {
            service_id: "fixture-stable-id".into(),
            summary: summary_from_configuration("old label", configuration.as_deref()).unwrap(),
            configuration,
        }
    }

    #[test]
    fn complete_dictionary_round_trip_preserves_hidden_attributes_and_absence() {
        let dictionary = editable_configuration(None).unwrap();
        set_string(&dictionary, "HTTPProxyUsername", "fixture secret").unwrap();
        set_string(&dictionary, "SOCKSProxy", "foreign.socks").unwrap();
        set_number(&dictionary, "SOCKSEnable", 1).unwrap();
        let before = fixture_snapshot(&dictionary);
        let after = before
            .with_change(&NativeProxyChange::Manual {
                host: "new.local",
                port: 7890,
                bypass: &["localhost".into()],
            })
            .unwrap();
        let decoded = decode_configuration(after.configuration.as_deref())
            .unwrap()
            .unwrap();
        assert!(string_equals(
            &decoded,
            "HTTPProxyUsername",
            "fixture secret"
        ));
        assert!(string_equals(&decoded, "SOCKSProxy", "foreign.socks"));
        assert_eq!(number(&decoded, "SOCKSEnable"), 1);
        assert_eq!(number(&decoded, "HTTPEnable"), 1);
        assert!(!before.same_state(&after).unwrap());
        let restored = decode_configuration(before.configuration.as_deref())
            .unwrap()
            .unwrap();
        let round_trip = fixture_snapshot(&restored);
        assert!(before.same_state(&round_trip).unwrap());
        let key = OwnedCf::string("HTTPEnable").unwrap();
        // SAFETY: `restored` is a live decoded dictionary and `key` is a live CFString.
        assert!(unsafe { CFDictionaryGetValue(restored.pointer(), key.pointer()) }.is_null());
        assert!(!configurations_equal(None, before.configuration.as_deref()).unwrap());
    }

    #[test]
    fn hidden_external_changes_and_integer_to_real_changes_fail_complete_equality() {
        let original = editable_configuration(None).unwrap();
        set_number(&original, "NativeExtra", 1).unwrap();
        let before = fixture_snapshot(&original);
        let changed = editable_configuration(before.configuration.as_deref()).unwrap();
        let key = OwnedCf::string("NativeExtra").unwrap();
        let real = 1.0_f64;
        // SAFETY: Test creates one live CFNumber with kCFNumberFloat64Type (6).
        let value = unsafe {
            OwnedCf::created(
                CFNumberCreate(ptr::null(), 6, (&raw const real).cast()),
                "fixture real",
            )
        }
        .unwrap();
        // SAFETY: `changed` is a live mutable dictionary and both objects are live.
        unsafe { CFDictionarySetValue(changed.0.as_ptr(), key.pointer(), value.pointer()) };
        assert!(!before.same_state(&fixture_snapshot(&changed)).unwrap());
        set_number(&changed, "NativeExtra", 1).unwrap();
        set_string(&changed, "UnknownCredential", "external").unwrap();
        assert!(!before.same_state(&fixture_snapshot(&changed)).unwrap());
    }

    #[test]
    fn stable_identity_survives_a_display_name_change() {
        let dictionary = editable_configuration(None).unwrap();
        let before = fixture_snapshot(&dictionary);
        let mut renamed = before.clone();
        renamed.summary.service = "new label".into();
        assert!(before.same_target(&renamed));
        assert!(before.same_state(&renamed).unwrap());
    }
}
