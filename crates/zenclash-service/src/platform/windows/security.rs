use std::{
    fs, io,
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
    ptr,
};

#[cfg(any(test, feature = "server"))]
use windows_sys::Win32::Security::{
    Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, SECURITY_ATTRIBUTES,
};
#[cfg(feature = "server")]
use windows_sys::Win32::{Foundation::ERROR_ALREADY_EXISTS, Storage::FileSystem::CreateDirectoryW};
use windows_sys::Win32::{
    Foundation::{GENERIC_ALL, GENERIC_WRITE, LocalFree},
    Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT},
    Security::{
        ACCESS_ALLOWED_ACE, ACCESS_DENIED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce,
        INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    },
    Storage::FileSystem::{
        DELETE, FILE_APPEND_DATA, FILE_ATTRIBUTE_REPARSE_POINT, FILE_WRITE_ATTRIBUTES,
        FILE_WRITE_DATA, FILE_WRITE_EA, WRITE_DAC, WRITE_OWNER,
    },
    System::Com::CoTaskMemFree,
    UI::Shell::{FOLDERID_ProgramData, SHGetKnownFolderPath},
};
#[cfg(feature = "server")]
use windows_sys::Win32::{
    Security::{
        Authorization::{GetSecurityInfo, SE_SERVICE},
        IsValidAcl, IsValidSid,
    },
    System::Services::{SC_HANDLE, SERVICE_CHANGE_CONFIG},
};

use super::{denied, identity::sid_string, wide};

#[cfg(feature = "server")]
pub(super) const PRIVATE_SDDL: &str = "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
#[cfg(feature = "server")]
pub(super) const PUBLIC_SDDL: &str =
    "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GRGX;;;BU)";

pub(super) struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    #[cfg(any(test, feature = "server"))]
    pub(super) fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text = wide(sddl)?;
        let mut descriptor = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    #[cfg(any(test, feature = "server"))]
    pub(super) fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

pub(crate) fn service_root() -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    let mut text = ptr::null_mut();
    let result =
        unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, 0, ptr::null_mut(), &mut text) };
    if result < 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    let mut len = 0;
    let path = unsafe {
        while *text.add(len) != 0 {
            len += 1;
        }
        PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(
            text, len,
        )))
    };
    unsafe { CoTaskMemFree(text.cast()) };
    Ok(path.join("ZenClashService"))
}

#[cfg(feature = "server")]
pub(crate) fn create_private_directory(path: &Path, public_read: bool) -> io::Result<()> {
    let descriptor = Descriptor::from_sddl(if public_read {
        PUBLIC_SDDL
    } else {
        PRIVATE_SDDL
    })?;
    let text = wide(path)?;
    if unsafe { CreateDirectoryW(text.as_ptr(), &descriptor.attributes()) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(error);
        }
    }
    validate_protected_path(path, true)
}

pub(crate) fn validate_protected_path(path: &Path, is_dir: bool) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(denied("service path must be absolute"));
    }
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(denied("service path contains a reparse point"));
        }
        validate_acl(ancestor, ancestor == path)?;
    }
    if fs::symlink_metadata(path)?.is_dir() != is_dir {
        return Err(denied("incorrect protected file type"));
    }
    Ok(())
}

fn validate_acl(path: &Path, leaf: bool) -> io::Result<()> {
    let text = wide(path)?;
    let mut owner = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut raw = ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            text.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut raw,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let _descriptor = Descriptor(raw);
    if !trusted_sid(&sid_string(owner)?) || dacl.is_null() {
        return Err(denied("service object is not administrator protected"));
    }
    // SAFETY: dacl is owned by the still-live descriptor returned by Windows.
    for index in 0..u32::from(unsafe { (*dacl).AceCount }) {
        let mut raw_ace = ptr::null_mut();
        if unsafe { GetAce(dacl, index, &mut raw_ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let header = unsafe { &*raw_ace.cast::<ACE_HEADER>() };
        // Standard ProgramData / drive root have CREATOR OWNER inheritance-only
        // entries. They grant rights to future children, not to this directory.
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        match header.AceType {
            0 => {
                let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
                let sid = std::ptr::addr_of!(ace.SidStart).cast_mut().cast();
                let forbidden = if leaf { write_mask() } else { namespace_mask() };
                if !trusted_sid(&sid_string(sid)?) && ace.Mask & forbidden != 0 {
                    return Err(denied(
                        "unprivileged account can modify protected service object",
                    ));
                }
            }
            1 => {
                let _ = unsafe { &*raw_ace.cast::<ACCESS_DENIED_ACE>() };
            }
            _ => return Err(denied("unsupported protected object ACE")),
        }
    }
    Ok(())
}

fn trusted_sid(sid: &str) -> bool {
    matches!(
        sid,
        "S-1-5-18"
            | "S-1-5-32-544"
            | "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
    )
}

// Adapted from upstream check_service_registration/review_security (GPL-3.0).
#[cfg(feature = "server")]
pub(super) fn validate_service_registration(handle: SC_HANDLE) -> io::Result<()> {
    let mut owner = ptr::null_mut();
    let mut dacl = ptr::null_mut();
    let mut raw = ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_SERVICE,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut raw,
        )
    };
    let _descriptor = Descriptor(raw);
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    if raw.is_null() {
        return Err(denied("SCM security descriptor is missing"));
    }
    // SAFETY: These pointers are owned by the still-live Windows descriptor.
    unsafe { review_service_security(owner, dacl) }
}

#[cfg(feature = "server")]
unsafe fn review_service_security(
    owner: windows_sys::Win32::Security::PSID,
    dacl: *mut ACL,
) -> io::Result<()> {
    if owner.is_null()
        || unsafe { IsValidSid(owner) } == 0
        || !trusted_sid(&sid_string(owner)?)
        || dacl.is_null()
        || unsafe { IsValidAcl(dacl) } == 0
    {
        return Err(denied("SCM registration is not administrator protected"));
    }
    let dangerous =
        SERVICE_CHANGE_CONFIG | DELETE | WRITE_DAC | WRITE_OWNER | GENERIC_WRITE | GENERIC_ALL;
    for index in 0..u32::from(unsafe { (*dacl).AceCount }) {
        let mut raw_ace = ptr::null_mut();
        if unsafe { GetAce(dacl, index, &mut raw_ace) } == 0 || raw_ace.is_null() {
            return Err(denied("SCM registration DACL is unreadable"));
        }
        let header = unsafe { &*raw_ace.cast::<ACE_HEADER>() };
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        match header.AceType {
            1 | 10 => continue,
            // Callback allows share the SID layout; their condition may authorize access.
            0 | 9 => {}
            _ => return Err(denied("unsupported SCM registration ACE")),
        }
        if usize::from(header.AceSize) < std::mem::size_of::<ACCESS_ALLOWED_ACE>() {
            return Err(denied("SCM registration ACE is truncated"));
        }
        let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
        if ace.Mask & dangerous == 0 {
            continue;
        }
        let sid: windows_sys::Win32::Security::PSID = ptr::addr_of!(ace.SidStart).cast_mut().cast();
        let sid_offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
        let sid_bytes = usize::from(header.AceSize) - sid_offset;
        if sid_bytes < 8 || 8 + usize::from(unsafe { *sid.cast::<u8>().add(1) }) * 4 > sid_bytes {
            return Err(denied("SCM registration ACE SID is truncated"));
        }
        if unsafe { IsValidSid(sid) } == 0 || !trusted_sid(&sid_string(sid)?) {
            return Err(denied("unprivileged account can change SCM registration"));
        }
    }
    Ok(())
}

// Creating sibling files/directories in shared ProgramData is permitted. Its
// namespace must remain fixed: unprivileged accounts cannot rename an ancestor,
// remove protected children, change the DACL, or replace its owner.
fn namespace_mask() -> u32 {
    GENERIC_ALL | DELETE | WRITE_DAC | WRITE_OWNER | 0x40
}

fn write_mask() -> u32 {
    GENERIC_ALL
        | GENERIC_WRITE
        | DELETE
        | WRITE_DAC
        | WRITE_OWNER
        | FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_WRITE_EA
        | FILE_WRITE_ATTRIBUTES
        | 0x40
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "server")]
    fn review_sddl(sddl: &str) -> io::Result<()> {
        use windows_sys::Win32::Security::{GetSecurityDescriptorDacl, GetSecurityDescriptorOwner};
        let descriptor = Descriptor::from_sddl(sddl)?;
        let mut owner = ptr::null_mut();
        let mut dacl = ptr::null_mut();
        let mut defaulted = 0;
        let mut present = 0;
        if unsafe { GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted) } == 0
            || unsafe {
                GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted)
            } == 0
        {
            return Err(io::Error::last_os_error());
        }
        unsafe { review_service_security(owner, dacl) }
    }

    #[cfg(feature = "server")]
    #[test]
    fn registration_security_rejects_untrusted_owner_null_and_unsupported_acl() {
        for sddl in [
            "O:BUD:P(A;;GA;;;SY)",
            "O:SYD:NO_ACCESS_CONTROL",
            "O:SYD:P(OA;;0x2;;;BU)",
        ] {
            Descriptor::from_sddl(sddl).unwrap();
            let calls = std::cell::Cell::new(0);
            let result = review_sddl(sddl).map(|()| calls.set(calls.get() + 1));
            assert!(result.is_err(), "untrusted SCM descriptor accepted: {sddl}");
            assert_eq!(calls.get(), 0);
        }
    }

    #[cfg(feature = "server")]
    #[test]
    fn registration_security_rejects_each_unprivileged_mutation_right() {
        for right in [
            SERVICE_CHANGE_CONFIG,
            DELETE,
            WRITE_DAC,
            WRITE_OWNER,
            GENERIC_WRITE,
            GENERIC_ALL,
        ] {
            let sddl = format!("O:SYD:P(A;;GA;;;SY)(A;;0x{right:08X};;;BU)");
            let calls = std::cell::Cell::new(0);
            let result = review_sddl(&sddl).map(|()| calls.set(calls.get() + 1));
            assert!(result.is_err(), "SCM right 0x{right:X} accepted");
            assert_eq!(calls.get(), 0);
        }
    }

    #[cfg(feature = "server")]
    #[test]
    fn registration_security_allows_trusted_and_readonly_or_empty_dacl() {
        for sddl in [
            "O:BAD:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GR;;;BU)",
            "O:SYD:P",
            "O:SYD:P(D;;GA;;;BU)(A;;GA;;;SY)",
        ] {
            review_sddl(sddl).unwrap();
        }
    }

    #[cfg(feature = "server")]
    #[test]
    fn registration_security_invalid_native_handle_is_not_approval() {
        assert!(validate_service_registration(ptr::null_mut()).is_err());
    }

    #[test]
    fn user_owned_file_is_rejected_even_without_reparse_points() {
        let path =
            std::env::temp_dir().join(format!("zenclash-untrusted-{}.txt", std::process::id()));
        fs::write(&path, b"not a protected service").unwrap();
        let result = validate_protected_path(&path, false);
        fs::remove_file(path).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn current_process_identity_is_native_and_detects_birth_mismatch() {
        let identity = super::super::identity::current_identity().unwrap();
        assert!(super::super::identity::peer_alive(&identity));
        let recycled = crate::session::PeerIdentity::new(
            identity.user().into(),
            identity.pid(),
            identity.birth() + 1,
        );
        assert!(!super::super::identity::peer_alive(&recycled));
    }

    #[test]
    fn standard_programdata_ancestors_have_a_stable_administrator_namespace() {
        let root = service_root().unwrap();
        for ancestor in root.parent().unwrap().ancestors() {
            validate_acl(ancestor, false).unwrap();
        }
    }
}
