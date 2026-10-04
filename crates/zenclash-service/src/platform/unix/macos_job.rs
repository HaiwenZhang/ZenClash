//! Typed, fixed-label launchd PID observation; missing data remains unknown.
use std::io;

pub(super) fn parse_host_pid(code: Option<i32>, stdout: &str, stderr: &str) -> io::Result<u32> {
    let pid = stdout
        .strip_suffix('\n')
        .filter(|text| {
            !text.is_empty() && text.len() <= 10 && text.bytes().all(|byte| byte.is_ascii_digit())
        })
        .and_then(|text| text.parse::<u32>().ok())
        .filter(|pid| *pid > 0 && *pid <= i32::MAX as u32);
    if code != Some(0) || !stderr.is_empty() {
        return Err(io::Error::other("loaded job PID query is unconfirmed"));
    }
    pid.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "loaded job PID output is invalid",
        )
    })
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use std::ffi::{c_char, c_void};
    type CfRef = *const c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: CfRef,
            string: *const c_char,
            encoding: u32,
        ) -> CfRef;
        fn CFRelease(value: CfRef);
        fn CFGetTypeID(value: CfRef) -> usize;
        fn CFDictionaryGetTypeID() -> usize;
        fn CFDictionaryGetValue(dictionary: CfRef, key: CfRef) -> CfRef;
        fn CFNumberGetTypeID() -> usize;
        fn CFNumberIsFloatType(number: CfRef) -> u8;
        fn CFNumberGetValue(number: CfRef, kind: isize, output: *mut c_void) -> u8;
    }
    #[link(name = "ServiceManagement", kind = "framework")]
    unsafe extern "C" {
        static kSMDomainSystemLaunchd: CfRef;
        fn SMJobCopyDictionary(domain: CfRef, label: CfRef) -> CfRef;
    }

    struct OwnedCf(CfRef);
    impl Drop for OwnedCf {
        fn drop(&mut self) {
            // SAFETY: each non-null Create/Copy result is released exactly once.
            unsafe { CFRelease(self.0) };
        }
    }
    fn string(value: &'static std::ffi::CStr) -> io::Result<OwnedCf> {
        // SAFETY: static C string is NUL terminated; null allocator selects default.
        let value =
            unsafe { CFStringCreateWithCString(std::ptr::null(), value.as_ptr(), 0x0800_0100) };
        if value.is_null() {
            return Err(io::Error::other("CoreFoundation string allocation failed"));
        }
        Ok(OwnedCf(value))
    }
    pub(in crate::platform) fn query_fixed_host_pid() -> io::Result<u32> {
        let label = string(c"org.zenclash.service")?;
        let key = string(c"PID")?;
        // SAFETY: system domain is an Apple framework constant, label is owned/live.
        let dictionary = unsafe { SMJobCopyDictionary(kSMDomainSystemLaunchd, label.0) };
        if dictionary.is_null() {
            return Err(io::Error::other("loaded job description is unconfirmed"));
        }
        let dictionary = OwnedCf(dictionary);
        dictionary_pid(dictionary.0, key.0)
    }
    fn dictionary_pid(dictionary: CfRef, key: CfRef) -> io::Result<u32> {
        if dictionary.is_null() || key.is_null() {
            return Err(io::Error::other("loaded job dictionary is missing"));
        }
        // SAFETY: Copy returns a live CF object; check its runtime type before access.
        if unsafe { CFGetTypeID(dictionary) != CFDictionaryGetTypeID() } {
            return Err(io::Error::other("loaded job description type is invalid"));
        }
        // SAFETY: dictionary has CFDictionary type and key is a live CFString.
        let number = unsafe { CFDictionaryGetValue(dictionary, key) };
        if number.is_null() {
            return Err(io::Error::other("loaded job PID is missing"));
        }
        // SAFETY: borrowed value remains alive while the dictionary is owned.
        if unsafe { CFGetTypeID(number) != CFNumberGetTypeID() || CFNumberIsFloatType(number) != 0 }
        {
            return Err(io::Error::other("loaded job PID type is invalid"));
        }
        let mut pid = 0i64;
        // SAFETY: kCFNumberSInt64Type=4 requests an exactly sized i64 output; false
        // means conversion was lossy, so no returned bytes are accepted on failure.
        if unsafe { CFNumberGetValue(number, 4, (&mut pid as *mut i64).cast()) } == 0 {
            return Err(io::Error::other("loaded job PID conversion failed"));
        }
        u32::try_from(pid)
            .ok()
            .filter(|pid| *pid > 0 && *pid <= i32::MAX as u32)
            .ok_or_else(|| io::Error::other("loaded job PID is not a live process identifier"))
    }

    #[cfg(test)]
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFDictionaryCreateMutable(
            allocator: CfRef,
            capacity: isize,
            keys: *const c_void,
            values: *const c_void,
        ) -> CfRef;
        fn CFDictionarySetValue(dictionary: CfRef, key: CfRef, value: CfRef);
        fn CFNumberCreate(allocator: CfRef, kind: isize, value: *const c_void) -> CfRef;
    }
    #[cfg(test)]
    #[test]
    fn typed_dictionary_requires_integral_positive_pid_and_correct_runtime_types() {
        let key = string(c"PID").unwrap();
        assert!(dictionary_pid(std::ptr::null(), key.0).is_err());
        let wrong = string(c"123").unwrap();
        assert!(dictionary_pid(wrong.0, key.0).is_err());
        // SAFETY: null callbacks make a pointer-key dictionary; fixtures retain
        // all borrowed keys/values until each dictionary lookup has finished.
        let dictionary = unsafe {
            CFDictionaryCreateMutable(std::ptr::null(), 0, std::ptr::null(), std::ptr::null())
        };
        assert!(!dictionary.is_null());
        let dictionary = OwnedCf(dictionary);
        assert!(dictionary_pid(dictionary.0, key.0).is_err());
        // SAFETY: owned mutable dictionary and both fixture values remain live.
        unsafe { CFDictionarySetValue(dictionary.0, key.0, wrong.0) };
        assert!(dictionary_pid(dictionary.0, key.0).is_err());
        for value in [0i64, -1, i32::MAX as i64 + 1, i64::MAX, 123] {
            // SAFETY: SInt64 type reads the exact initialized i64 fixture value.
            let number =
                unsafe { CFNumberCreate(std::ptr::null(), 4, (&value as *const i64).cast()) };
            assert!(!number.is_null());
            let number = OwnedCf(number);
            // SAFETY: dictionary and the new borrowed value are alive.
            unsafe { CFDictionarySetValue(dictionary.0, key.0, number.0) };
            if value == 123 {
                assert_eq!(dictionary_pid(dictionary.0, key.0).unwrap(), 123);
            } else {
                assert!(dictionary_pid(dictionary.0, key.0).is_err());
            }
        }
        let floating = 123.5f64;
        // SAFETY: Float64 type reads the exact initialized f64 fixture value.
        let number =
            unsafe { CFNumberCreate(std::ptr::null(), 6, (&floating as *const f64).cast()) };
        assert!(!number.is_null());
        let number = OwnedCf(number);
        // SAFETY: dictionary and the floating-point fixture remain alive.
        unsafe { CFDictionarySetValue(dictionary.0, key.0, number.0) };
        assert!(dictionary_pid(dictionary.0, key.0).is_err());
    }
}
#[cfg(target_os = "macos")]
pub(super) use native::query_fixed_host_pid;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_pid_helper_output_requires_complete_positive_pid_and_clean_success() {
        assert_eq!(parse_host_pid(Some(0), "123\n", "").unwrap(), 123);
        for output in [
            "",
            "0\n",
            "-1\n",
            "+123\n",
            "123",
            "123\n123\n",
            "123.0\n",
            "2147483648\n",
            " 123\n",
        ] {
            assert!(parse_host_pid(Some(0), output, "").is_err(), "{output:?}");
        }
        assert!(parse_host_pid(Some(1), "123\n", "").is_err());
        assert!(parse_host_pid(None, "123\n", "").is_err());
        assert!(parse_host_pid(Some(0), "123\n", "\n").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn fixed_pid_capture_rejects_stderr_failure_and_timeout_without_native_effects() {
        use std::time::Duration;
        let command = |script: &str| {
            let mut command = tokio::process::Command::new("/bin/sh");
            command.args(["-c", script]);
            command
        };
        assert_eq!(
            super::super::launchd_probe::capture_host_pid(
                command("printf '123\\n'"),
                Duration::from_secs(1)
            )
            .unwrap(),
            123
        );
        for script in [
            "printf '123\\n'; printf 'warning' >&2",
            "printf '123\\n'; exit 1",
            "printf '0\\n'",
            "printf '\\377'",
            "exec sleep 10",
        ] {
            assert!(
                super::super::launchd_probe::capture_host_pid(
                    command(script),
                    Duration::from_millis(100)
                )
                .is_err()
            );
        }
    }
}
