//! Windows per-user SID, security-descriptor, and private-file ACL helpers.

use anyhow::{Context as _, Result, bail};
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
use std::path::Path;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_EXISTS, GENERIC_READ, GENERIC_WRITE,
    INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
    GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    GetTokenInformation, IsValidSid, IsWellKnownSid, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
    SECURITY_ATTRIBUTES, TOKEN_INFORMATION_CLASS, TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER, TokenOwner,
    TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_TYPE_DISK, GetFileInformationByHandle, GetFileType, OPEN_EXISTING, READ_CONTROL,
    WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

pub(crate) fn current_sid() -> Result<String> {
    let buffer = query_process_token(TokenUser, "SID")?;
    // SAFETY: `query_process_token` succeeded with `TokenUser` into a `usize`-aligned buffer
    // sized by `GetTokenInformation`, so its start holds an initialized `TOKEN_USER`.
    let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    sid_to_string(token_user.User.Sid)
}

fn current_token_owner_matches(file: &std::fs::File) -> Result<bool> {
    let buffer = query_process_token(TokenOwner, "owner SID")?;
    // SAFETY: `query_process_token` succeeded with `TokenOwner` into a `usize`-aligned buffer
    // sized by `GetTokenInformation`, so its start holds an initialized `TOKEN_OWNER`.
    let token_owner = unsafe { &*buffer.as_ptr().cast::<TOKEN_OWNER>() };
    if token_owner.Owner.is_null() {
        return Ok(false);
    }
    owner_matches(file, token_owner.Owner)
}

fn query_process_token(info_class: TOKEN_INFORMATION_CLASS, label: &str) -> Result<Vec<usize>> {
    let mut token = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess()` returns the current process pseudo-handle and `token` is
    // valid writable storage for the opened token handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error()).context("failed to open process token");
    }
    let token = OwnedHandle(token);

    let mut required = 0_u32;
    // SAFETY: `token.0` is a live `TOKEN_QUERY` handle; passing a null buffer with length 0
    // queries the required byte size into `required`.
    unsafe { GetTokenInformation(token.0, info_class, std::ptr::null_mut(), 0, &mut required) };
    if required == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to size process {label} buffer"));
    }
    let words = (required as usize).div_ceil(std::mem::size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    // SAFETY: `token.0` is a live `TOKEN_QUERY` handle and `buffer` provides at least `required`
    // bytes of `usize`-aligned writable storage.
    if unsafe {
        GetTokenInformation(
            token.0,
            info_class,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to read process {label}"));
    }
    Ok(buffer)
}

pub(crate) fn create_private_file(path: &Path) -> Result<std::fs::File> {
    let sid = current_sid()?;
    let descriptor = LocalSecurityDescriptor::from_sid(&sid)?;
    create_file(path, &descriptor)
}

pub(crate) fn protect_file(path: &Path, create: bool) -> Result<std::fs::File> {
    let descriptor = LocalSecurityDescriptor::from_sid(&current_sid()?)?;
    let expected_owner = descriptor.owner()?;
    let open = || {
        open_no_reparse(
            path,
            false,
            GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC,
        )
    };
    let file = if create {
        match create_file(path, &descriptor) {
            Ok(file) => file,
            Err(error) if is_os_error(&error, ERROR_FILE_EXISTS) => open()?,
            Err(error) => return Err(error),
        }
    } else {
        open()?
    };
    if !owner_matches(&file, expected_owner)? {
        // Elevated Windows processes default new file ownership (e.g., SQLite sidecars) to
        // TokenOwner (BUILTIN\Administrators) instead of TokenUser. Normalize those files to
        // the current user while still refusing files owned by any other account.
        if !current_token_owner_matches(&file)? {
            bail!("refusing to change another user's file permissions");
        }
        take_file_ownership(path, &file, expected_owner)?;
    }
    descriptor.apply_dacl(file.as_raw_handle())?;
    validate_private_file(&file, expected_owner)?;
    Ok(file)
}

pub(crate) fn open_private_file(path: &Path) -> Result<std::fs::File> {
    let sid = current_sid()?;
    let descriptor = LocalSecurityDescriptor::from_sid(&sid)?;
    let file = open_file(path)?;
    validate_private_file(&file, descriptor.owner()?)?;
    Ok(file)
}

pub(crate) fn open_or_create_private_file(path: &Path) -> Result<std::fs::File> {
    let sid = current_sid()?;
    let descriptor = LocalSecurityDescriptor::from_sid(&sid)?;
    match create_file(path, &descriptor) {
        Ok(file) => Ok(file),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.raw_os_error() == Some(ERROR_FILE_EXISTS as i32)) =>
        {
            let file = open_file(path)?;
            validate_private_file(&file, descriptor.owner()?)?;
            Ok(file)
        }
        Err(error) => Err(error),
    }
}

fn create_file(path: &Path, descriptor: &LocalSecurityDescriptor) -> Result<std::fs::File> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_raw(),
        bInheritHandle: 0,
    };
    let wide = wide_path(path)?;
    // SAFETY: `wide` is NUL-terminated UTF-16 and `attributes` points to `descriptor`, both of
    // which remain valid for the duration of `CreateFileW`.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error())
            .context("failed to create private current-user file");
    }
    // SAFETY: `CreateFileW` succeeded and returned an owned, non-invalid file handle.
    let file = unsafe { std::fs::File::from_raw_handle(handle) };
    validate_private_file(&file, descriptor.owner()?)?;
    Ok(file)
}

fn open_file(path: &Path) -> Result<std::fs::File> {
    let wide = wide_path(path)?;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path valid for the duration of `CreateFileW`.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error())
            .context("failed to open private current-user file");
    }
    // SAFETY: `CreateFileW` succeeded and returned an owned, non-invalid file handle.
    let file = unsafe { std::fs::File::from_raw_handle(handle) };
    Ok(file)
}

fn validate_private_file(file: &std::fs::File, expected_owner: PSID) -> Result<()> {
    let handle = file.as_raw_handle();
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `handle` belongs to live `file` and `information` is valid writable storage.
    if unsafe { GetFileInformationByHandle(handle, &mut information) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to inspect private file metadata");
    }
    // SAFETY: `handle` belongs to live `file`.
    let file_type = unsafe { GetFileType(handle) };
    if information.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || file_type != FILE_TYPE_DISK
    {
        bail!("current-user file is not an ordinary file");
    }

    let mut owner = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut security = std::ptr::null_mut();
    // SAFETY: `handle` belongs to live `file` and all out-pointers reference stack storage;
    // the allocated `security` descriptor is freed via `LocalSecurityDescriptor`.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut security,
        )
    };
    if status != 0 || security.is_null() {
        bail!("failed to inspect private file security: Windows error {status}");
    }
    let security = LocalSecurityDescriptor(security);
    let owner_matches_expected = if owner.is_null() {
        false
    } else {
        // SAFETY: `owner` points into the live `security` descriptor and `expected_owner` is a
        // valid SID supplied by the caller.
        unsafe { EqualSid(owner, expected_owner) != 0 }
    };
    if !owner_matches_expected || dacl.is_null() {
        bail!("current-user file owner or DACL is invalid");
    }
    let mut control = 0_u16;
    let mut revision = 0_u32;
    // SAFETY: `security.0` is a valid security descriptor and `control`/`revision` are writable.
    if unsafe { GetSecurityDescriptorControl(security.0, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
    {
        bail!("current-user file DACL is not protected");
    }

    let mut owner_ace = false;
    let mut system_ace = false;
    let mut administrators_ace = false;
    // SAFETY: `dacl` is non-null and points to a valid `ACL` inside the live `security` descriptor.
    let ace_count = unsafe { (*dacl).AceCount };
    for index in 0..u32::from(ace_count) {
        let mut ace = std::ptr::null_mut();
        // SAFETY: `dacl` is valid while `security` lives, `index < ace_count`, and `ace` is writable.
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
            bail!("current-user file DACL could not be inspected");
        }
        // SAFETY: `GetAce` succeeded and returned a non-null ACE header within `dacl`.
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        if allowed.Header.AceType != ACCESS_ALLOWED_ACE_TYPE
            || allowed.Mask & FILE_ALL_ACCESS != FILE_ALL_ACCESS
        {
            bail!("current-user file DACL contains an unexpected ACE");
        }
        let ace_sid = std::ptr::addr_of!(allowed.SidStart)
            .cast_mut()
            .cast::<c_void>();
        // SAFETY: `ace_sid` points to the inline SID in `ACCESS_ALLOWED_ACE` inside live `dacl`.
        if unsafe { IsValidSid(ace_sid) } == 0 {
            bail!("current-user file DACL contains an invalid SID");
        }
        // SAFETY: `ace_sid` was validated by `IsValidSid` and `expected_owner` is a valid SID.
        let is_owner = unsafe { EqualSid(ace_sid, expected_owner) } != 0;
        let is_system = if is_owner {
            false
        } else {
            // SAFETY: `ace_sid` was validated by `IsValidSid` and stays live with `security`.
            unsafe { IsWellKnownSid(ace_sid, WinLocalSystemSid) != 0 }
        };
        let is_administrators = if is_owner || is_system {
            false
        } else {
            // SAFETY: `ace_sid` was validated by `IsValidSid` and stays live with `security`.
            unsafe { IsWellKnownSid(ace_sid, WinBuiltinAdministratorsSid) != 0 }
        };
        if is_owner {
            owner_ace = true;
        } else if is_system {
            system_ace = true;
        } else if is_administrators {
            administrators_ace = true;
        } else {
            bail!("current-user file DACL grants access to another SID");
        }
    }
    if ace_count != 3 || !owner_ace || !system_ace || !administrators_ace {
        bail!("current-user file DACL is missing a required principal");
    }
    Ok(())
}

pub(crate) fn is_access_denied(error: &anyhow::Error) -> bool {
    is_os_error(error, ERROR_ACCESS_DENIED)
}

pub(crate) fn is_os_error(error: &anyhow::Error, code: u32) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::raw_os_error)
        == Some(code as i32)
}

fn take_file_ownership(path: &Path, existing: &std::fs::File, expected: PSID) -> Result<()> {
    let file = open_no_reparse(path, false, WRITE_OWNER)?;
    if !same_file(existing, &file)? {
        bail!("file changed while normalizing private current-user file owner");
    }
    // SAFETY: `file` is an open handle with `WRITE_OWNER` access and `expected` is a valid SID.
    let status = unsafe {
        SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            expected,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32))
            .context("failed to set private current-user file owner");
    }
    Ok(())
}

fn same_file(left: &std::fs::File, right: &std::fs::File) -> Result<bool> {
    let mut left_info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `left` is an open file handle and `left_info` is valid writable storage.
    if unsafe { GetFileInformationByHandle(left.as_raw_handle(), &mut left_info) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to inspect existing private file identity");
    }
    let mut right_info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `right` is an open file handle and `right_info` is valid writable storage.
    if unsafe { GetFileInformationByHandle(right.as_raw_handle(), &mut right_info) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to inspect reopened private file identity");
    }
    Ok(
        left_info.dwVolumeSerialNumber == right_info.dwVolumeSerialNumber
            && left_info.nFileIndexHigh == right_info.nFileIndexHigh
            && left_info.nFileIndexLow == right_info.nFileIndexLow,
    )
}

pub(crate) fn take_directory_ownership(path: &Path, expected: PSID) -> Result<()> {
    let directory = open_no_reparse(path, true, WRITE_OWNER)?;
    // SAFETY: `directory` is an open handle with `WRITE_OWNER` access and `expected` is a valid SID.
    let status = unsafe {
        SetSecurityInfo(
            directory.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            expected,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32))
            .context("failed to set the application data root owner");
    }
    Ok(())
}

pub(crate) fn owner_matches(directory: &std::fs::File, expected: PSID) -> Result<bool> {
    let mut owner = std::ptr::null_mut();
    let mut security = std::ptr::null_mut();
    // SAFETY: `directory` is an open handle with `READ_CONTROL` access; `owner` and `security`
    // are valid out-pointers and `security` is owned by `LocalSecurityDescriptor` below.
    let status = unsafe {
        GetSecurityInfo(
            directory.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut security,
        )
    };
    if status != 0 || security.is_null() {
        bail!("failed to inspect the application data root owner: Windows error {status}");
    }
    let security = LocalSecurityDescriptor(security);
    let matches = if owner.is_null() {
        false
    } else {
        // SAFETY: `owner` points into the live `security` descriptor and `expected` is a valid SID.
        unsafe { EqualSid(owner, expected) != 0 }
    };
    drop(security);
    Ok(matches)
}

pub(crate) fn open_no_reparse(path: &Path, directory: bool, access: u32) -> Result<std::fs::File> {
    let wide = wide_path(path)?;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path valid for the duration of `CreateFileW`.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to open {path:?}"));
    }
    // SAFETY: `CreateFileW` succeeded and returned an owned, non-invalid handle.
    let file = unsafe { std::fs::File::from_raw_handle(handle) };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` owns a live handle and `information` is valid writable storage.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to inspect {path:?}"));
    }
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
    {
        bail!("{path:?} is not the kind of object the owner repair expects");
    }
    Ok(file)
}

fn sid_to_string(sid: PSID) -> Result<String> {
    let mut value = std::ptr::null_mut();
    // SAFETY: `sid` is a valid SID pointer and `value` receives a LocalAlloc-allocated NUL-terminated
    // wide string freed by `LocalWideString`.
    if unsafe { ConvertSidToStringSidW(sid, &mut value) } == 0 || value.is_null() {
        return Err(std::io::Error::last_os_error()).context("failed to format process SID");
    }
    let value = LocalWideString(value);
    let length = (0..)
        .take_while(|index| {
            // SAFETY: `value.0` is a NUL-terminated UTF-16 buffer allocated by
            // `ConvertSidToStringSidW`, so stepping until the first NUL stays in bounds.
            unsafe { *value.0.add(*index) != 0 }
        })
        .count();
    // SAFETY: `value.0` points to `length` initialized `u16` elements valid while `value` lives.
    let units = unsafe { std::slice::from_raw_parts(value.0, length) };
    String::from_utf16(units).context("process SID is not valid UTF-16")
}

pub(crate) fn wide_path(path: &Path) -> Result<Vec<u16>> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        bail!("owner token path contains NUL");
    }
    wide.push(0);
    Ok(wide)
}

struct OwnedHandle(*mut c_void);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: `self.0` is an exclusively owned valid Windows handle closed once on drop.
            unsafe { CloseHandle(self.0) };
        }
    }
}

struct LocalWideString(*mut u16);

impl Drop for LocalWideString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` was allocated by Windows (`ConvertSidToStringSidW`) and is freed once.
            unsafe { LocalFree(self.0.cast()) };
        }
    }
}

pub(crate) struct LocalSecurityDescriptor(pub(crate) PSECURITY_DESCRIPTOR);

impl LocalSecurityDescriptor {
    pub(crate) fn from_sid(sid: &str) -> Result<Self> {
        let sddl = format!("O:{sid}D:P(A;;FA;;;{sid})(A;;FA;;;SY)(A;;FA;;;BA)");
        let mut wide: Vec<u16> = sddl.encode_utf16().collect();
        wide.push(0);
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: `wide` is a NUL-terminated UTF-16 SDDL string and `descriptor` receives a
        // LocalAlloc-allocated security descriptor owned by `Self`.
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
            return Err(std::io::Error::last_os_error())
                .context("failed to build owner token security descriptor");
        }
        Ok(Self(descriptor))
    }

    pub(crate) const fn as_raw(&self) -> PSECURITY_DESCRIPTOR {
        self.0
    }

    pub(crate) fn owner(&self) -> Result<PSID> {
        let mut owner = std::ptr::null_mut();
        let mut defaulted = 0;
        // SAFETY: `self.0` is a valid security descriptor and `owner`/`defaulted` are writable.
        if unsafe { GetSecurityDescriptorOwner(self.0, &mut owner, &mut defaulted) } == 0
            || owner.is_null()
        {
            return Err(std::io::Error::last_os_error())
                .context("failed to read token descriptor owner");
        }
        Ok(owner)
    }

    pub(crate) fn apply_dacl(&self, handle: *mut c_void) -> Result<()> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        // SAFETY: `self.0` is a valid security descriptor and all out-pointers are writable.
        if unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) }
            == 0
            || present == 0
            || dacl.is_null()
        {
            return Err(std::io::Error::last_os_error())
                .context("failed to read token descriptor DACL");
        }
        // SAFETY: `handle` is an open object handle with `WRITE_DAC` access and `dacl` points to
        // a valid `ACL` inside live `self.0`.
        let status = unsafe {
            SetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        if status != 0 {
            bail!("failed to restrict owner token DACL: Windows error {status}");
        }
        Ok(())
    }
}

impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` was allocated by Windows (`GetSecurityInfo` or
            // `ConvertStringSecurityDescriptorToSecurityDescriptorW`) and is freed once on drop.
            unsafe { LocalFree(self.0) };
        }
    }
}
