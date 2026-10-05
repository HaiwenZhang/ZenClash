// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Checks that privileged core execution and asset reads use protected locations.
//! Owner authentication alone cannot establish trust: any local account may become an owner.

use crate::ServiceErrorCode;
use crate::core::auth::ServiceError;
use std::path::{Path, PathBuf};

/// Rejects a core the requesting account could have written.
///
/// `canonical` is expected to already be a canonicalized ordinary file.
pub(crate) fn require_trusted_core_location(canonical: &Path) -> Result<(), ServiceError> {
    // Integration tests stage cores in temporary directories that deliberately fail this rule.
    if cfg!(feature = "test") {
        return Ok(());
    }
    check_platform_location(canonical)
}

pub(crate) fn untrusted(message: impl Into<String>) -> ServiceError {
    ServiceError::new(ServiceErrorCode::InvalidInstallLocation, message)
}

/// Validates a source for automatic core staging and returns its canonical path.
/// Callers must copy from this returned path to avoid retargetable links.
/// Untrusted sources require explicit `--install-core` with a `--sha256` attestation.
///
/// # Errors
/// Returns errors canonicalizing the source or verifying its protected approved installation.
pub fn require_trusted_core_source(path: &Path) -> anyhow::Result<PathBuf> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| anyhow::anyhow!("failed to canonicalize core source {path:?}: {error}"))?;
    require_trusted_core_location(&canonical)?;
    Ok(canonical)
}

#[cfg(target_os = "macos")]
fn check_platform_location(canonical: &Path) -> Result<(), ServiceError> {
    // Stop at the protected state root: macOS makes its parent admin-group-writable.
    let paths = crate::core::paths::service_paths().map_err(|error| untrusted(error.to_string()))?;
    let state_root = paths.persistent_state_dir();
    if canonical.starts_with(state_root) {
        return require_root_owned_chain(canonical, Some(state_root));
    }
    if !is_trusted_macos_location(canonical) {
        return Err(untrusted(format!(
            "core path {canonical:?} is under neither {state_root:?} nor {MACOS_APPLICATIONS_ROOT}"
        )));
    }
    // Reject group/other-writable components below /Applications; its admin-writable root is exempt.
    use std::os::unix::fs::MetadataExt as _;
    for component in canonical.ancestors() {
        if component == Path::new(MACOS_APPLICATIONS_ROOT) {
            break;
        }
        let metadata = std::fs::symlink_metadata(component)
            .map_err(|error| untrusted(format!("failed to inspect core path {component:?}: {error}")))?;
        if metadata.mode() & 0o022 != 0 {
            return Err(untrusted(format!(
                "core path {component:?} is writable by group or other"
            )));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
const MACOS_APPLICATIONS_ROOT: &str = "/Applications";

/// Allow system Applications only; bundles may be owned by the user who installed them.
#[cfg(target_os = "macos")]
fn is_trusted_macos_location(canonical: &Path) -> bool {
    canonical.starts_with(MACOS_APPLICATIONS_ROOT)
}

/// Every component from the core down to `/` must be root-owned and unwritable by group or other.
/// Packaged installs land in root-owned prefixes, so this holds for DEB and RPM builds.
#[cfg(target_os = "linux")]
fn check_platform_location(canonical: &Path) -> Result<(), ServiceError> {
    require_root_owned_chain(canonical, None)
}

/// Requires every component up the tree to be root-owned and unwritable by group or other.
///
/// `stop_after` bounds the walk at a directory whose own parents the operating system manages, for
/// platforms where those parents are deliberately group-writable.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn require_root_owned_chain(canonical: &Path, stop_after: Option<&Path>) -> Result<(), ServiceError> {
    use std::os::unix::fs::MetadataExt as _;

    for component in canonical.ancestors() {
        let metadata = std::fs::symlink_metadata(component)
            .map_err(|error| untrusted(format!("failed to inspect core path {component:?}: {error}")))?;
        if metadata.uid() != 0 {
            return Err(untrusted(format!("core path {component:?} is not owned by root")));
        }
        if metadata.mode() & 0o022 != 0 {
            return Err(untrusted(format!(
                "core path {component:?} is writable by group or other"
            )));
        }
        if stop_after == Some(component) {
            break;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn check_platform_location(canonical: &Path) -> Result<(), ServiceError> {
    windows_location::check(canonical)
}

#[cfg(all(windows, not(feature = "test")))]
pub(crate) fn require_trusted_service_registration(
    service: &platform_lib::service::Service,
) -> Result<(), ServiceError> {
    windows_location::check_service_registration(service)
}

#[cfg(windows)]
mod windows_location {
    use super::{ServiceError, untrusted};
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
    use std::path::{Component, Path, Prefix};
    use windows_sys::Win32::Foundation::{GENERIC_ALL, GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSidToSidW, GetSecurityInfo, SE_FILE_OBJECT, SE_OBJECT_TYPE,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce, IsValidSid, IsWellKnownSid,
        OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, WinBuiltinAdministratorsSid, WinLocalSystemSid,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, DELETE, FILE_ALL_ACCESS, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_DELETE_CHILD,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, OPEN_EXISTING, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
    };

    /// `NT SERVICE\TrustedInstaller`, the owner Windows leaves on `%ProgramFiles%`.
    const TRUSTED_INSTALLER_SID: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const VOLUME_ROOT_MINIMAL_CAPABILITY_SID: &str =
        "S-1-15-3-65536-1888954469-739942743-1668119174-2468466756-4239452838-1296943325-355587736-700089176";
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;
    const ACCESS_ALLOWED_CALLBACK_ACE_TYPE: u8 = 9;
    const ACCESS_DENIED_CALLBACK_ACE_TYPE: u8 = 10;
    const INHERIT_ONLY_ACE_FLAG: u8 = 0x08;

    /// Rights that let a principal put different bytes behind the core's own path.
    const FILE_HIJACK_RIGHTS: u32 = DELETE
        | FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_WRITE_EA
        | FILE_WRITE_ATTRIBUTES
        | WRITE_DAC
        | WRITE_OWNER
        | GENERIC_WRITE
        | GENERIC_ALL;

    /// Rights that allow replacing a directory on the core path.
    /// Exclude add-subdirectory: stock C:\ grants it to Authenticated Users, but it cannot replace a child.
    const DIRECTORY_HIJACK_RIGHTS: u32 = DELETE | FILE_DELETE_CHILD | WRITE_DAC | WRITE_OWNER | GENERIC_ALL;

    pub(super) fn check(canonical: &Path) -> Result<(), ServiceError> {
        let trusted = TrustedSids::new()?;
        // `ancestors` yields the core itself first, then every directory up to the volume root.
        for (index, component) in canonical.ancestors().enumerate() {
            let is_core = index == 0;
            let hijack_rights = if is_core {
                FILE_HIJACK_RIGHTS
            } else {
                DIRECTORY_HIJACK_RIGHTS
            };
            let handle = open_for_security_review(component, !is_core)?;
            review_component(handle.as_raw_handle(), component, hijack_rights, &trusted)?;
        }
        Ok(())
    }

    fn review_component(
        handle: *mut c_void,
        component: &Path,
        hijack_rights: u32,
        trusted: &TrustedSids,
    ) -> Result<(), ServiceError> {
        let label = format!("core path {component:?}");
        let security = SecurityInfo::read(handle, SE_FILE_OBJECT, &label)?;
        review_security(
            &security,
            hijack_rights,
            trusted,
            &label,
            is_local_volume_root(component),
        )
    }

    fn is_local_volume_root(path: &Path) -> bool {
        let mut components = path.components();
        matches!(components.next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
            && matches!(components.next(), Some(Component::RootDir))
            && components.next().is_none()
    }

    #[cfg(not(feature = "test"))]
    pub(super) fn check_service_registration(service: &platform_lib::service::Service) -> Result<(), ServiceError> {
        use windows_sys::Win32::Security::Authorization::SE_SERVICE;

        let label = format!("registered service {:?}", crate::WINDOWS_SERVICE_NAME);
        let security = SecurityInfo::read(service.raw_handle().cast(), SE_SERVICE, &label)?;
        let rights = platform_lib::service::ServiceAccess::CHANGE_CONFIG.bits()
            | DELETE
            | WRITE_DAC
            | WRITE_OWNER
            | GENERIC_WRITE
            | GENERIC_ALL;
        review_security(&security, rights, &TrustedSids::new()?, &label, false)
    }

    fn review_security(
        security: &SecurityInfo,
        hijack_rights: u32,
        trusted: &TrustedSids,
        label: &str,
        volume_root: bool,
    ) -> Result<(), ServiceError> {
        if !trusted.contains(security.owner) {
            return Err(untrusted(format!(
                "{label} is owned by an account other than SYSTEM, Administrators or TrustedInstaller"
            )));
        }
        // A null DACL grants everyone everything; an empty one denies everyone.
        if security.dacl.is_null() {
            return Err(untrusted(format!("{label} has no DACL")));
        }
        // A volume root cannot be deleted or renamed, so DELETE on it cannot move the core path.
        let inert_rights = if volume_root { DELETE } else { 0 };

        for index in 0..u32::from(unsafe { (*security.dacl).AceCount }) {
            let mut ace = std::ptr::null_mut();
            if unsafe { GetAce(security.dacl, index, &mut ace) } == 0 || ace.is_null() {
                return Err(untrusted(format!("{label} has an unreadable DACL")));
            }
            let header = unsafe { *ace.cast::<ACE_HEADER>() };
            // Inherit-only entries do not apply to this object, only to children it later gains.
            if header.AceFlags & INHERIT_ONLY_ACE_FLAG != 0 {
                continue;
            }
            match header.AceType {
                ACCESS_DENIED_ACE_TYPE | ACCESS_DENIED_CALLBACK_ACE_TYPE => continue,
                // Callback allows have the same SID layout; assume their condition can hold.
                ACCESS_ALLOWED_ACE_TYPE | ACCESS_ALLOWED_CALLBACK_ACE_TYPE => {}
                // Object ACEs have a different SID layout; unknown types remain untrusted.
                _ => {
                    return Err(untrusted(format!(
                        "{label} carries an ACE type this check cannot evaluate"
                    )));
                }
            }
            let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            let dangerous = allowed.Mask & hijack_rights & !inert_rights;
            if dangerous == 0 {
                continue;
            }
            let sid = std::ptr::addr_of!(allowed.SidStart).cast_mut().cast::<c_void>();
            if !trusted.contains(sid) {
                // AppContainer capabilities intersect with user/group access. This stock
                // isolatedWin32-volumeRootMinimal ACE cannot independently authorize a write.
                // Keep the exception at the volume root; other principals and children
                // must still pass the ordinary checks.
                if volume_root
                    && hijack_rights == DIRECTORY_HIJACK_RIGHTS
                    && header.AceType == ACCESS_ALLOWED_ACE_TYPE
                    && header.AceFlags == 0x03
                    && allowed.Mask == FILE_ALL_ACCESS
                    && unsafe { IsValidSid(sid) } != 0
                    && unsafe { EqualSid(sid, LocalSid::from_string(VOLUME_ROOT_MINIMAL_CAPABILITY_SID)?.as_ptr()) }
                        != 0
                {
                    continue;
                }
                return Err(untrusted(format!(
                    "{label} has an untrusted write ACE: ace_index={index}, sid={}, type={}, flags=0x{:02X}, mask=0x{:08X}, dangerous=0x{:08X}",
                    sid_string(sid),
                    header.AceType,
                    header.AceFlags,
                    allowed.Mask,
                    dangerous
                )));
            }
        }
        Ok(())
    }

    fn sid_string(sid: PSID) -> String {
        let mut wide = std::ptr::null_mut();
        if unsafe { IsValidSid(sid) } == 0 || unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 {
            return "<invalid SID>".to_owned();
        }
        let mut len = 0;
        unsafe {
            while *wide.add(len) != 0 {
                len += 1;
            }
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(wide, len));
            LocalFree(wide.cast());
            value
        }
    }

    fn open_for_security_review(path: &Path, directory: bool) -> Result<std::fs::File, ServiceError> {
        let wide = wide_path(path)?;
        let flags = FILE_FLAG_OPEN_REPARSE_POINT
            | if directory {
                FILE_FLAG_BACKUP_SEMANTICS
            } else {
                FILE_ATTRIBUTE_NORMAL
            };
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                READ_CONTROL,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                flags,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(untrusted(format!("core path {path:?} could not be opened for review")));
        }
        Ok(unsafe { std::fs::File::from_raw_handle(handle) })
    }

    fn wide_path(path: &Path) -> Result<Vec<u16>, ServiceError> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            return Err(untrusted("core path contains NUL"));
        }
        wide.push(0);
        Ok(wide)
    }

    struct TrustedSids {
        trusted_installer: LocalSid,
    }

    impl TrustedSids {
        fn new() -> Result<Self, ServiceError> {
            Ok(Self {
                trusted_installer: LocalSid::from_string(TRUSTED_INSTALLER_SID)?,
            })
        }

        fn contains(&self, sid: PSID) -> bool {
            if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
                return false;
            }
            unsafe {
                IsWellKnownSid(sid, WinLocalSystemSid) != 0
                    || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
                    || EqualSid(sid, self.trusted_installer.as_ptr()) != 0
            }
        }
    }

    struct LocalSid(*mut c_void);

    impl LocalSid {
        fn from_string(value: &str) -> Result<Self, ServiceError> {
            let mut wide: Vec<u16> = value.encode_utf16().collect();
            wide.push(0);
            let mut sid = std::ptr::null_mut();
            if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) } == 0 || unsafe { IsValidSid(sid) } == 0 {
                return Err(untrusted(format!("SID {value} could not be built")));
            }
            Ok(Self(sid))
        }

        fn as_ptr(&self) -> PSID {
            self.0
        }
    }

    impl Drop for LocalSid {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { LocalFree(self.0) };
            }
        }
    }

    struct SecurityInfo {
        owner: PSID,
        dacl: *mut ACL,
        _descriptor: LocalSecurityDescriptor,
    }

    impl SecurityInfo {
        fn read(handle: *mut c_void, object_type: SE_OBJECT_TYPE, label: &str) -> Result<Self, ServiceError> {
            let mut owner = std::ptr::null_mut();
            let mut dacl = std::ptr::null_mut();
            let mut descriptor = std::ptr::null_mut();
            let status = unsafe {
                GetSecurityInfo(
                    handle,
                    object_type,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    &mut owner,
                    std::ptr::null_mut(),
                    &mut dacl,
                    std::ptr::null_mut(),
                    &mut descriptor,
                )
            };
            if status != 0 || descriptor.is_null() {
                return Err(untrusted(format!(
                    "{label} security could not be inspected (Win32 {status})"
                )));
            }
            Ok(Self {
                owner,
                dacl,
                _descriptor: LocalSecurityDescriptor(descriptor),
            })
        }
    }

    struct LocalSecurityDescriptor(PSECURITY_DESCRIPTOR);

    impl Drop for LocalSecurityDescriptor {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { LocalFree(self.0) };
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::{GetSecurityDescriptorDacl, GetSecurityDescriptorOwner};

        const REPORTED_CAPABILITY: &str =
            "S-1-15-3-65536-1888954469-739942743-1668119174-2468466756-4239452838-1296943325-355587736-700089176";

        fn reported_root() -> String {
            format!(
                "O:{TRUSTED_INSTALLER_SID}G:{TRUSTED_INSTALLER_SID}D:\
                 (A;OICIIO;0x1301bf;;;AU)(A;;0x100004;;;AU)(A;OICI;FA;;;SY)\
                 (A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)\
                 (A;OICI;FA;;;{REPORTED_CAPABILITY})(A;;0x1000a1;;;{REPORTED_CAPABILITY})"
            )
        }

        fn review(sddl: &str, path: Option<&Path>, rights: u32) -> Result<(), ServiceError> {
            let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
            let mut descriptor = std::ptr::null_mut();
            assert_ne!(
                unsafe {
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        wide.as_ptr(),
                        SDDL_REVISION_1,
                        &mut descriptor,
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            let descriptor = LocalSecurityDescriptor(descriptor);
            let mut owner = std::ptr::null_mut();
            let mut defaulted = 0;
            assert_ne!(
                unsafe { GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted) },
                0
            );
            let mut dacl = std::ptr::null_mut();
            let mut present = 0;
            assert_ne!(
                unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) },
                0
            );
            let security = SecurityInfo {
                owner,
                dacl,
                _descriptor: descriptor,
            };
            review_security(
                &security,
                rights,
                &TrustedSids::new()?,
                "reported path",
                path.is_some_and(is_local_volume_root),
            )
        }

        #[test]
        fn accepts_reported_volume_root_capability_without_relaxing_other_aces() {
            let sddl = reported_root();
            for root in [r"C:\", r"\\?\C:\", r"D:\"] {
                review(&sddl, Some(Path::new(root)), DIRECTORY_HIJACK_RIGHTS).unwrap();
            }
            for sid in ["WD", "AU", "BU", "S-1-5-21-1-2-3-1001"] {
                for mask in ["FA", "0x40", "0x40000", "0x80000"] {
                    let unsafe_sddl = format!("{sddl}(A;;{mask};;;{sid})");
                    assert!(review(&unsafe_sddl, Some(Path::new(r"C:\")), DIRECTORY_HIJACK_RIGHTS).is_err());
                }
            }
        }

        #[test]
        fn capability_exception_cannot_authorize_other_locations_or_owners() {
            let sddl = reported_root();
            for path in [
                r"C:\ProgramData",
                r"C:\cores\mihomo.exe",
                r"\\server\share\",
                r"C:",
                r"\",
                r"C:\folder\..",
            ] {
                assert!(
                    review(&sddl, Some(Path::new(path)), DIRECTORY_HIJACK_RIGHTS).is_err(),
                    "{path}"
                );
            }
            assert!(review(&sddl, None, DIRECTORY_HIJACK_RIGHTS).is_err());
            assert!(review(&sddl, Some(Path::new(r"C:\")), FILE_HIJACK_RIGHTS).is_err());
            let owner = format!("O:{REPORTED_CAPABILITY}G:SYD:(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
            assert!(review(&owner, Some(Path::new(r"C:\")), DIRECTORY_HIJACK_RIGHTS).is_err());
            let unknown = sddl.replace(REPORTED_CAPABILITY, "S-1-15-3-65536-1-2-3-4-5-6-7-8");
            assert!(review(&unknown, Some(Path::new(r"C:\")), DIRECTORY_HIJACK_RIGHTS).is_err());
            let different_flags = sddl.replace("(A;OICI;FA;;;S-1-15", "(A;;FA;;;S-1-15");
            assert!(review(&different_flags, Some(Path::new(r"C:\")), DIRECTORY_HIJACK_RIGHTS).is_err());
        }

        #[test]
        fn delete_is_inert_only_on_the_volume_root() {
            // A reported C:\ ACL: Authenticated Users with Modify on the root itself.
            let sddl = "O:SYG:SYD:(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;;0x1301bf;;;AU)";
            review(sddl, Some(Path::new(r"\\?\C:\")), DIRECTORY_HIJACK_RIGHTS).unwrap();
            assert!(review(sddl, Some(Path::new(r"C:\ProgramData")), DIRECTORY_HIJACK_RIGHTS).is_err());
        }

        #[test]
        fn rejection_identifies_the_untrusted_ace() {
            let sddl = format!("{}(A;;0x40000;;;AU)", reported_root());
            let error = review(&sddl, Some(Path::new(r"C:\")), DIRECTORY_HIJACK_RIGHTS).unwrap_err();
            let message = error.to_string();
            for detail in [
                "ace_index=7",
                "sid=S-1-5-11",
                "mask=0x00040000",
                "flags=0x00",
                "dangerous=0x00040000",
            ] {
                assert!(message.contains(detail), "missing {detail}: {message}");
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::is_trusted_macos_location;
    use std::path::Path;

    #[test]
    fn accepts_a_core_inside_the_applications_bundle() {
        assert!(is_trusted_macos_location(Path::new(
            "/Applications/ZenClash.app/Contents/MacOS/mihomo"
        )));
    }

    #[test]
    fn rejects_the_user_writable_home_applications_directory() {
        assert!(!is_trusted_macos_location(Path::new(
            "/Users/someone/Applications/ZenClash.app/Contents/MacOS/mihomo"
        )));
    }

    #[test]
    fn rejects_a_sibling_directory_that_merely_shares_the_prefix() {
        assert!(!is_trusted_macos_location(Path::new("/Applications-elsewhere/mihomo")));
    }

    #[test]
    fn rejects_an_arbitrary_writable_path() {
        assert!(!is_trusted_macos_location(Path::new("/tmp/mihomo")));
    }
}

#[cfg(all(test, not(feature = "test"), target_os = "macos"))]
mod production_gate_tests {
    use super::require_trusted_core_location;
    use std::path::Path;

    #[test]
    fn the_production_gate_rejects_a_core_outside_applications() {
        assert!(require_trusted_core_location(Path::new("/tmp/mihomo")).is_err());
        // Use a real root-owned path to exercise the production metadata check.
        assert!(require_trusted_core_location(Path::new("/Applications/Utilities")).is_ok());
        assert!(
            require_trusted_core_location(Path::new("/Applications/does-not-exist.app/Contents/MacOS/mihomo")).is_err()
        );
    }
}
