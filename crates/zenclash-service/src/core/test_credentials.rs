// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
#[cfg(windows)]
use crate::OWNER_TOKEN_FILE_NAME;
use crate::{OwnerCredentials, OwnerIdentity};
use anyhow::Result;
use std::path::Path;

#[cfg(unix)]
pub(crate) const SYNTHETIC_TEST_OWNER_TOKEN_PREFIX: &str = "zenclash-service-test-owner:";

///
/// # Errors
/// Returns errors preparing and securing the isolated test owner credentials.
pub fn test_owner_credentials(app_data_root: &Path) -> Result<OwnerCredentials> {
    std::fs::create_dir_all(app_data_root)?;

    #[cfg(unix)]
    let (identity, token) = (
        OwnerIdentity::Unix {
            uid: unsafe { platform_lib::geteuid() },
            gid: unsafe { platform_lib::getegid() },
        },
        None,
    );
    #[cfg(windows)]
    let (identity, token) = windows::create(app_data_root)?;

    Ok(OwnerCredentials {
        identity,
        app_data_dir: app_data_root.to_string_lossy().into_owned(),
        token,
    })
}

/// Creates synthetic test owner credentials for the given Unix user ID.
///
/// # Errors
/// Returns an error if the application data directory cannot be created.
#[cfg(unix)]
pub fn test_owner_credentials_for_uid(app_data_root: &Path, uid: u32) -> Result<OwnerCredentials> {
    std::fs::create_dir_all(app_data_root)?;
    let identity = OwnerIdentity::Unix {
        uid,
        gid: unsafe { platform_lib::getegid() },
    };
    let token = format!("{SYNTHETIC_TEST_OWNER_TOKEN_PREFIX}{}", crate::owner_key(&identity));
    Ok(OwnerCredentials {
        identity,
        app_data_dir: app_data_root.to_string_lossy().into_owned(),
        token: Some(token),
    })
}

#[cfg(windows)]
mod windows {
    use super::{OWNER_TOKEN_FILE_NAME, OwnerIdentity};
    use anyhow::{Context as _, Result, bail};
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetTokenInformation, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SetFileSecurityW, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    const TOKEN_BYTES: usize = 32;

    pub(super) fn create(root: &Path) -> Result<(OwnerIdentity, Option<String>)> {
        let sid = current_sid()?;
        let descriptor = SecurityDescriptor::for_owner(&sid)?;
        descriptor.apply(root)?;

        let token = [0xa5_u8; TOKEN_BYTES];
        let token_path = root.join(OWNER_TOKEN_FILE_NAME);
        std::fs::write(&token_path, token).context("failed to write test owner token")?;
        descriptor.apply(&token_path)?;

        Ok((OwnerIdentity::Windows { sid }, Some(encode_token(&token))))
    }

    fn current_sid() -> Result<String> {
        let mut token = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(std::io::Error::last_os_error()).context("failed to open process token");
        }
        let token = OwnedHandle(token);

        let mut required = 0_u32;
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut required) };
        if required == 0 {
            return Err(std::io::Error::last_os_error()).context("failed to size process SID buffer");
        }
        let words = (required as usize).div_ceil(std::mem::size_of::<usize>());
        let mut buffer = vec![0_usize; words];
        if unsafe { GetTokenInformation(token.0, TokenUser, buffer.as_mut_ptr().cast(), required, &mut required) } == 0
        {
            return Err(std::io::Error::last_os_error()).context("failed to read process SID");
        }
        let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        sid_to_string(token_user.User.Sid)
    }

    fn sid_to_string(sid: *mut c_void) -> Result<String> {
        let mut value = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut value) } == 0 || value.is_null() {
            return Err(std::io::Error::last_os_error()).context("failed to format process SID");
        }
        let value = LocalWideString(value);
        let length = (0..).take_while(|index| unsafe { *value.0.add(*index) } != 0).count();
        String::from_utf16(unsafe { std::slice::from_raw_parts(value.0, length) })
            .context("process SID is not valid UTF-16")
    }

    fn encode_token(token: &[u8; TOKEN_BYTES]) -> String {
        let mut encoded = String::with_capacity(TOKEN_BYTES * 2);
        for byte in token {
            use std::fmt::Write as _;
            let _ = write!(encoded, "{byte:02x}");
        }
        encoded
    }

    fn wide_path(path: &Path) -> Result<Vec<u16>> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            bail!("test credential path contains NUL");
        }
        wide.push(0);
        Ok(wide)
    }

    struct OwnedHandle(*mut c_void);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    struct LocalWideString(*mut u16);

    impl Drop for LocalWideString {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { LocalFree(self.0.cast()) };
            }
        }
    }

    struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

    impl SecurityDescriptor {
        fn for_owner(sid: &str) -> Result<Self> {
            let sddl = format!("O:{sid}D:P(A;;FA;;;{sid})(A;;FA;;;SY)(A;;FA;;;BA)");
            let mut wide: Vec<u16> = sddl.encode_utf16().collect();
            wide.push(0);
            let mut descriptor = std::ptr::null_mut();
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            } == 0
                || descriptor.is_null()
            {
                return Err(std::io::Error::last_os_error()).context("failed to create test owner security descriptor");
            }
            Ok(Self(descriptor))
        }

        fn apply(&self, path: &Path) -> Result<()> {
            let wide = wide_path(path)?;
            if unsafe {
                SetFileSecurityW(
                    wide.as_ptr(),
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    self.0,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("failed to secure test owner path {path:?}"));
            }
            Ok(())
        }
    }

    impl Drop for SecurityDescriptor {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { LocalFree(self.0) };
            }
        }
    }
}
