use std::{
    ffi::c_void,
    ptr::{self, NonNull},
};

use super::super::SystemProxyStatus;
use crate::{MihomoError, MihomoResult};

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
    fn CFStringCreateWithBytes(
        allocator: CfRef,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        external: u8,
    ) -> CfRef;
    fn CFStringCompare(left: CfRef, right: CfRef, options: usize) -> isize;
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
    #[cfg(test)]
    fn CFDictionaryGetValue(dictionary: CfRef, key: CfRef) -> CfRef;
    #[cfg(test)]
    fn CFNumberGetValue(number: CfRef, kind: isize, output: *mut c_void) -> u8;
}

#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    static kSCNetworkProtocolTypeProxies: CfRef;
    fn SCError() -> i32;
    fn SCPreferencesCreateWithAuthorization(
        allocator: CfRef,
        name: CfRef,
        identifier: CfRef,
        authorization: CfRef,
    ) -> CfRef;
    fn SCPreferencesLock(preferences: CfRef, wait: u8) -> u8;
    fn SCPreferencesUnlock(preferences: CfRef) -> u8;
    fn SCPreferencesCommitChanges(preferences: CfRef) -> u8;
    fn SCPreferencesApplyChanges(preferences: CfRef) -> u8;
    fn SCNetworkServiceCopyAll(preferences: CfRef) -> CfRef;
    fn SCNetworkServiceGetName(service: CfRef) -> CfRef;
    fn SCNetworkServiceCopyProtocol(service: CfRef, kind: CfRef) -> CfRef;
    fn SCNetworkProtocolGetConfiguration(protocol: CfRef) -> CfRef;
    fn SCNetworkProtocolSetConfiguration(protocol: CfRef, config: CfRef) -> u8;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn AuthorizationCreate(
        rights: CfRef,
        environment: CfRef,
        flags: u32,
        authorization: *mut CfRef,
    ) -> i32;
    fn AuthorizationFree(authorization: CfRef, flags: u32) -> i32;
}

struct Authorization(NonNull<c_void>);

impl Authorization {
    fn create() -> MihomoResult<Self> {
        let mut authorization = ptr::null();
        // SAFETY: Null rights/environment and kAuthorizationFlagDefaults (0)
        // create a reference without requesting rights or displaying UI. The
        // live output buffer receives one reference owned until AuthorizationFree.
        let status =
            unsafe { AuthorizationCreate(ptr::null(), ptr::null(), 0, &raw mut authorization) };
        if status != 0 {
            return Err(operation_error("AuthorizationCreate", status));
        }
        NonNull::new(authorization.cast_mut())
            .map(Self)
            .ok_or_else(|| operation_error("AuthorizationCreate", -60002))
    }
}

impl Drop for Authorization {
    fn drop(&mut self) {
        // SAFETY: This non-null reference came from one successful Create;
        // default flags release local resources without destroying shared rights.
        if unsafe { AuthorizationFree(self.0.as_ptr(), 0) } != 0 {
            tracing::warn!("failed to release native proxy recovery authorization");
        }
    }
}

struct OwnedCf(NonNull<c_void>);

impl OwnedCf {
    // SAFETY: A non-null pointer must be a valid +1 Create/Copy result. This
    // function consumes that reference; borrowed Get results must not enter it.
    unsafe fn created(pointer: CfRef, operation: &str) -> MihomoResult<Self> {
        NonNull::new(pointer.cast_mut())
            .map(Self)
            .ok_or_else(|| native_error(operation))
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

struct PreferencesLock<'a> {
    preferences: &'a OwnedCf,
    locked: bool,
}

impl Drop for PreferencesLock<'_> {
    fn drop(&mut self) {
        if self.locked {
            // SAFETY: The borrowed owner outlives this acquired lock. Even an
            // unsuccessful restore releases exclusive native preference access.
            if unsafe { SCPreferencesUnlock(self.preferences.pointer()) } == 0 {
                tracing::warn!("failed to unlock native proxy recovery preferences");
            }
        }
    }
}

fn native_error(operation: &str) -> MihomoError {
    // SAFETY: SCError has no arguments or borrowed object lifetime.
    operation_error(operation, unsafe { SCError() })
}

fn operation_error(operation: &str, code: i32) -> MihomoError {
    MihomoError::Process(zenclash_i18n::text_with(
        "system_proxy.errors.snapshot_native",
        &[("operation", operation.into()), ("error", code.to_string())],
    ))
}

fn unique_service_index(matches: impl Iterator<Item = bool>) -> MihomoResult<usize> {
    let mut indices = matches
        .enumerate()
        .filter_map(|(index, matched)| matched.then_some(index));
    match (indices.next(), indices.next()) {
        (Some(index), None) => Ok(index),
        _ => Err(MihomoError::Process(zenclash_i18n::text(
            "system_proxy.errors.snapshot_service",
        ))),
    }
}

// SAFETY: Current must be null or a valid borrowed CFDictionary with retaining
// CFType callbacks, live throughout this synchronous copying call.
unsafe fn snapshot_dictionary(
    current: CfRef,
    previous: &SystemProxyStatus,
) -> MihomoResult<OwnedCf> {
    // SAFETY: A non-null current dictionary is borrowed from a live protocol;
    // empty configuration uses the SDK's retaining CFType callbacks instead.
    let pointer = unsafe {
        if current.is_null() {
            CFDictionaryCreateMutable(
                ptr::null(),
                0,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            )
        } else {
            CFDictionaryCreateMutableCopy(ptr::null(), 0, current)
        }
    };
    // SAFETY: The pointer is null or the +1 mutable Create/Copy result above.
    let dictionary = unsafe { OwnedCf::created(pointer, "CFDictionaryCreateMutableCopy") }?;
    for (key, value) in [
        ("HTTPEnable", i32::from(previous.enabled)),
        ("HTTPPort", i32::from(previous.port)),
        ("HTTPSEnable", i32::from(previous.secure_enabled)),
        ("HTTPSPort", i32::from(previous.secure_port)),
        ("ProxyAutoConfigEnable", i32::from(previous.auto_enabled)),
    ] {
        let key = OwnedCf::string(key)?;
        let value = OwnedCf::number(value)?;
        // SAFETY: Mutable copy and CFType callbacks retain both live objects.
        unsafe { CFDictionarySetValue(dictionary.0.as_ptr(), key.pointer(), value.pointer()) };
    }
    for (key, value) in [
        ("HTTPProxy", previous.server.as_str()),
        ("HTTPSProxy", previous.secure_server.as_str()),
        ("ProxyAutoConfigURLString", previous.auto_url.as_str()),
    ] {
        let key = OwnedCf::string(key)?;
        let value = OwnedCf::string(value)?;
        // SAFETY: The dictionary retains the live CFString key and value.
        unsafe { CFDictionarySetValue(dictionary.0.as_ptr(), key.pointer(), value.pointer()) };
    }
    let bypass = previous
        .bypass
        .iter()
        .map(|entry| OwnedCf::string(entry))
        .collect::<MihomoResult<Vec<_>>>()?;
    let pointers = bypass.iter().map(OwnedCf::pointer).collect::<Vec<_>>();
    // SAFETY: All pointers are live CFStrings; the retaining callbacks copy
    // their references before this temporary pointer array is dropped.
    let bypass = unsafe {
        OwnedCf::created(
            CFArrayCreate(
                ptr::null(),
                if pointers.is_empty() {
                    ptr::null()
                } else {
                    pointers.as_ptr()
                },
                pointers.len() as isize,
                &raw const kCFTypeArrayCallBacks,
            ),
            "CFArrayCreate",
        )
    }?;
    let key = OwnedCf::string("ExceptionsList")?;
    // SAFETY: Dictionary retains the live array and key. Empty arrays are valid.
    unsafe { CFDictionarySetValue(dictionary.0.as_ptr(), key.pointer(), bypass.pointer()) };
    Ok(dictionary)
}

pub(super) fn restore(previous: &SystemProxyStatus) -> MihomoResult<()> {
    let name = OwnedCf::string("ZenClash proxy snapshot recovery")?;
    let authorization = Authorization::create()?;
    // SAFETY: Null allocator/ID select documented defaults; name and the
    // authorization reference outlive this session. SystemConfiguration handles
    // the OS authorization policy for its writes, as for native system tools.
    let preferences = unsafe {
        OwnedCf::created(
            SCPreferencesCreateWithAuthorization(
                ptr::null(),
                name.pointer(),
                ptr::null(),
                authorization.0.as_ptr(),
            ),
            "SCPreferencesCreateWithAuthorization",
        )
    }?;
    // SAFETY: 0 means do not wait; preferences owns the live session.
    if unsafe { SCPreferencesLock(preferences.pointer(), 0) } == 0 {
        return Err(native_error("SCPreferencesLock"));
    }
    let mut lock = PreferencesLock {
        preferences: &preferences,
        locked: true,
    };
    // SAFETY: Copy returns a retained array from the live locked session.
    let services = unsafe {
        OwnedCf::created(
            SCNetworkServiceCopyAll(preferences.pointer()),
            "SCNetworkServiceCopyAll",
        )
    }?;
    let expected = OwnedCf::string(&previous.service)?;
    // SAFETY: Owned services is a CFArray. Every index remains within its count;
    // service/name Get results remain borrowed until that array is released.
    let count = unsafe { CFArrayGetCount(services.pointer()) };
    let index = unique_service_index((0..count).map(|index| unsafe {
        let service = CFArrayGetValueAtIndex(services.pointer(), index);
        let name = SCNetworkServiceGetName(service);
        !name.is_null() && CFStringCompare(name, expected.pointer(), 0) == 0
    }))?;
    // SAFETY: This index was selected from the array's bounded range. The
    // Proxies protocol constant is a system-owned CFString; Copy returns +1.
    let protocol = unsafe {
        OwnedCf::created(
            SCNetworkServiceCopyProtocol(
                CFArrayGetValueAtIndex(services.pointer(), index as isize),
                kSCNetworkProtocolTypeProxies,
            ),
            "SCNetworkServiceCopyProtocol",
        )
    }?;
    // SAFETY: Get returns a dictionary borrowed from the live protocol owner.
    let config = unsafe {
        snapshot_dictionary(
            SCNetworkProtocolGetConfiguration(protocol.pointer()),
            previous,
        )
    }?;
    // SAFETY: Only this locked session's uniquely selected Proxies protocol is
    // updated. Its retained dictionary lives through Set, Commit and Apply.
    if unsafe { SCNetworkProtocolSetConfiguration(protocol.pointer(), config.pointer()) } == 0 {
        return Err(native_error("SCNetworkProtocolSetConfiguration"));
    }
    // SAFETY: Lock and preferences remain live; permission/apply failures must
    // propagate so the controller retains the original recoverable snapshot.
    if unsafe { SCPreferencesCommitChanges(preferences.pointer()) } == 0 {
        return Err(native_error("SCPreferencesCommitChanges"));
    }
    // SAFETY: Same live preferences session; no wait-for-lock fallback.
    if unsafe { SCPreferencesApplyChanges(preferences.pointer()) } == 0 {
        return Err(native_error("SCPreferencesApplyChanges"));
    }
    // SAFETY: Exactly one successful Lock has been acquired in this session.
    if unsafe { SCPreferencesUnlock(preferences.pointer()) } == 0 {
        return Err(native_error("SCPreferencesUnlock"));
    }
    lock.locked = false;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_and_authorization_errors_remain_errors_without_native_writes() {
        // SAFETY: Null is an allowed failed Create result and owns no object.
        assert!(unsafe { OwnedCf::created(ptr::null(), "fixture allocation") }.is_err());
        assert!(
            operation_error("AuthorizationCreate", -60005)
                .to_string()
                .contains("-60005")
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

    #[test]
    fn cf_mapping_preserves_independent_protocol_flags_endpoints_and_pac_without_native_writes() {
        let previous = SystemProxyStatus {
            enabled: true,
            server: "http.original.test".into(),
            port: 8080,
            secure_enabled: false,
            secure_server: "https.cached.test".into(),
            secure_port: 8443,
            auto_enabled: true,
            auto_url: "http://pac.original.test/config.pac".into(),
            bypass: vec!["localhost".into(), "*.original.test".into()],
            ..Default::default()
        };
        // SAFETY: Null creates an in-memory dictionary with CFType callbacks.
        let dictionary = unsafe { snapshot_dictionary(ptr::null(), &previous) }.unwrap();
        assert_eq!(number(&dictionary, "HTTPEnable"), 1);
        assert_eq!(number(&dictionary, "HTTPSEnable"), 0);
        assert_eq!(number(&dictionary, "HTTPPort"), 8080);
        assert_eq!(number(&dictionary, "HTTPSPort"), 8443);
        assert_eq!(number(&dictionary, "ProxyAutoConfigEnable"), 1);
        assert!(string_equals(&dictionary, "HTTPProxy", &previous.server));
        assert!(string_equals(
            &dictionary,
            "HTTPSProxy",
            &previous.secure_server
        ));
        assert!(string_equals(
            &dictionary,
            "ProxyAutoConfigURLString",
            &previous.auto_url
        ));
        let key = OwnedCf::string("ExceptionsList").unwrap();
        // SAFETY: The retained array and its strings are owned by dictionary.
        unsafe {
            let bypass = CFDictionaryGetValue(dictionary.pointer(), key.pointer());
            assert!(!bypass.is_null());
            assert_eq!(CFArrayGetCount(bypass), 2);
            for (index, expected) in previous.bypass.iter().enumerate() {
                let expected = OwnedCf::string(expected).unwrap();
                assert_eq!(
                    CFStringCompare(
                        CFArrayGetValueAtIndex(bypass, index as isize),
                        expected.pointer(),
                        0
                    ),
                    0
                );
            }
        }
    }

    #[test]
    fn cf_mapping_restores_empty_disabled_endpoints_and_retains_unrelated_fields() {
        // SAFETY: Null creates only an in-memory dictionary, with no OS writes.
        let original =
            unsafe { snapshot_dictionary(ptr::null(), &SystemProxyStatus::default()) }.unwrap();
        let unrelated_key = OwnedCf::string("SOCKSProxy").unwrap();
        let unrelated_value = OwnedCf::string("unchanged.test").unwrap();
        // SAFETY: Test dictionary retains both live test strings.
        unsafe {
            CFDictionarySetValue(
                original.0.as_ptr(),
                unrelated_key.pointer(),
                unrelated_value.pointer(),
            )
        };
        // SAFETY: Original is a live CFType-callback dictionary owned by test.
        let restored =
            unsafe { snapshot_dictionary(original.pointer(), &SystemProxyStatus::default()) }
                .unwrap();
        assert_eq!(number(&restored, "HTTPEnable"), 0);
        assert_eq!(number(&restored, "HTTPPort"), 0);
        assert_eq!(number(&restored, "HTTPSPort"), 0);
        assert!(string_equals(&restored, "HTTPProxy", ""));
        assert!(string_equals(&restored, "HTTPSProxy", ""));
        assert!(string_equals(&restored, "ProxyAutoConfigURLString", ""));
        assert!(string_equals(&restored, "SOCKSProxy", "unchanged.test"));
    }
}
