//! Fixed SCM registration inspection, adapted from upstream paths/windows.rs.

use std::{
    ffi::OsString,
    io,
    os::windows::ffi::OsStringExt,
    path::{Component, Path, PathBuf},
};
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, LocalFree},
    System::Services::{
        QUERY_SERVICE_CONFIGW, QueryServiceConfigW, SC_HANDLE, SERVICE_WIN32_OWN_PROCESS,
    },
    UI::Shell::CommandLineToArgvW,
};

use super::super::{denied, wide};

const MAX_CONFIGURATION_BYTES: usize = 8192;

pub(super) fn check(handle: SC_HANDLE, expected: &Path) -> io::Result<()> {
    query_configuration_with(
        |buffer, length, needed| {
            if unsafe { QueryServiceConfigW(handle, buffer, length, needed) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        },
        |buffer| buffer.require_owned(expected),
    )
}

fn query_configuration_with(
    mut query: impl FnMut(*mut QUERY_SERVICE_CONFIGW, u32, &mut u32) -> io::Result<()>,
    review: impl FnOnce(&ConfigurationBuffer) -> io::Result<()>,
) -> io::Result<()> {
    let mut needed = 0;
    match query(std::ptr::null_mut(), 0, &mut needed) {
        Err(error) if error.raw_os_error() == Some(ERROR_INSUFFICIENT_BUFFER as i32) => {}
        Err(error) => return Err(error),
        Ok(()) => return Err(denied("SCM returned an empty configuration")),
    }
    let mut buffer = ConfigurationBuffer::new(needed as usize)?;
    let bytes = needed;
    query(buffer.words.as_mut_ptr().cast(), bytes, &mut needed)?;
    // pcbBytesNeeded is only specified on insufficient-buffer failure. On success,
    // review pointers against the bounded, fully initialized allocation we supplied.
    review(&buffer)
}

struct ConfigurationBuffer {
    words: Vec<usize>,
    bytes: usize,
}

impl ConfigurationBuffer {
    fn new(bytes: usize) -> io::Result<Self> {
        if !(std::mem::size_of::<QUERY_SERVICE_CONFIGW>()..=MAX_CONFIGURATION_BYTES)
            .contains(&bytes)
        {
            return Err(denied("SCM configuration exceeds its byte budget"));
        }
        Ok(Self {
            words: vec![0; bytes.div_ceil(std::mem::size_of::<usize>())],
            bytes,
        })
    }

    fn text(&self, pointer: *const u16, optional: bool) -> io::Result<OsString> {
        if pointer.is_null() && optional {
            return Ok(OsString::new());
        }
        let start = self.words.as_ptr() as usize;
        let address = pointer as usize;
        let offset = address
            .checked_sub(start)
            .ok_or_else(|| denied("SCM string is outside configuration"))?;
        if offset < std::mem::size_of::<QUERY_SERVICE_CONFIGW>()
            || offset >= self.bytes
            || !address.is_multiple_of(std::mem::align_of::<u16>())
        {
            return Err(denied("SCM string is outside configuration"));
        }
        // SAFETY: The checked pointer lies inside this initialized, aligned backing allocation.
        let text = unsafe { std::slice::from_raw_parts(pointer, (self.bytes - offset) / 2) };
        let length = text
            .iter()
            .position(|unit| *unit == 0)
            .ok_or_else(|| denied("SCM string is not terminated"))?;
        Ok(OsString::from_wide(&text[..length]))
    }

    fn require_owned(&self, expected: &Path) -> io::Result<()> {
        // SAFETY: Vec<usize> is aligned and has room for the initialized native header.
        let config = unsafe { &*self.words.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
        let command = self.text(config.lpBinaryPathName, false)?;
        let account = self.text(config.lpServiceStartName, false)?;
        let group = self.text(config.lpLoadOrderGroup, true)?;
        // No dependencies are registered by ZenClash. Read the first bounded entry only:
        // any nonempty MULTISZ is rejected without following further entries.
        let dependency = self.text(config.lpDependencies, true)?;
        if config.dwServiceType != SERVICE_WIN32_OWN_PROCESS
            || !(account.eq_ignore_ascii_case("LocalSystem")
                || account.eq_ignore_ascii_case(r"NT AUTHORITY\SYSTEM"))
            || !group.is_empty()
            || !dependency.is_empty()
        {
            return Err(denied(
                "registration is not the fixed own-process LocalSystem service",
            ));
        }
        require_fixed_executable(&command, expected)
    }
}

// Adapted from Tunglies' registered_executable (GPL-3.0); see UPSTREAM.md.
fn registered_executable(command: &std::ffi::OsStr) -> io::Result<PathBuf> {
    let text = wide(command)?;
    if text.len() == 1 {
        return Err(denied("registered service command is empty"));
    }
    let mut count = 0;
    let arguments = unsafe { CommandLineToArgvW(text.as_ptr(), &mut count) };
    if arguments.is_null() {
        return Err(io::Error::last_os_error());
    }
    let result = if count != 2 {
        Err(denied(
            "registered service command must name its executable and run",
        ))
    } else {
        // SAFETY: CommandLineToArgvW owns a NUL-terminated argument until LocalFree.
        let raw = unsafe { *arguments };
        let mut length = 0;
        while unsafe { *raw.add(length) } != 0 {
            length += 1;
        }
        let executable = PathBuf::from(OsString::from_wide(unsafe {
            std::slice::from_raw_parts(raw, length)
        }));
        let argument = unsafe { *arguments.add(1) };
        // SAFETY: The second argument has three UTF-16 units plus its terminator
        // only if each preceding unit matched; short-circuiting avoids overread.
        if unsafe { *argument } == u16::from(b'r')
            && unsafe { *argument.add(1) } == u16::from(b'u')
            && unsafe { *argument.add(2) } == u16::from(b'n')
            && unsafe { *argument.add(3) } == 0
        {
            Ok(executable)
        } else {
            Err(denied("registered service argument must be run"))
        }
    };
    unsafe { LocalFree(arguments.cast()) };
    result
}

fn require_fixed_executable(command: &std::ffi::OsStr, expected: &Path) -> io::Result<()> {
    let executable = registered_executable(command)?;
    if !executable.is_absolute()
        || executable
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        || executable.as_os_str() != expected.as_os_str()
    {
        return Err(denied(
            "registered executable does not match the fixed service image",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const EXPECTED: &str = r"C:\ProgramData\ZenClashService\zenclash-service.exe";
    const COMMAND: &str = r#""C:\ProgramData\ZenClashService\zenclash-service.exe" run"#;

    fn configuration(
        command: &str,
        account: &str,
        group: &str,
        dependency: &str,
        kind: u32,
    ) -> ConfigurationBuffer {
        let mut buffer = ConfigurationBuffer::new(1024).unwrap();
        let mut cursor = std::mem::size_of::<QUERY_SERVICE_CONFIGW>();
        let mut append = |text: &str| {
            let units = wide(text).unwrap();
            let pointer = unsafe {
                buffer
                    .words
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(cursor)
                    .cast::<u16>()
            };
            unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len()) };
            cursor += units.len() * 2;
            pointer
        };
        let mut header: QUERY_SERVICE_CONFIGW = unsafe { std::mem::zeroed() };
        header.dwServiceType = kind;
        header.lpBinaryPathName = append(command);
        header.lpServiceStartName = append(account);
        header.lpLoadOrderGroup = append(group);
        header.lpDependencies = append(dependency);
        unsafe {
            buffer
                .words
                .as_mut_ptr()
                .cast::<QUERY_SERVICE_CONFIGW>()
                .write(header)
        };
        buffer
    }

    #[test]
    fn registration_foreign_configuration_never_reaches_mutation() {
        for (command, account, group, dependency, kind) in [
            (
                r#""C:\foreign\zenclash-service.exe" run"#,
                "LocalSystem",
                "",
                "",
                SERVICE_WIN32_OWN_PROCESS,
            ),
            (
                COMMAND,
                r"NT AUTHORITY\LocalService",
                "",
                "",
                SERVICE_WIN32_OWN_PROCESS,
            ),
            (COMMAND, "LocalSystem", "", "", 0x20),
            (
                COMMAND,
                "LocalSystem",
                "foreign",
                "",
                SERVICE_WIN32_OWN_PROCESS,
            ),
            (
                COMMAND,
                "LocalSystem",
                "",
                "foreign",
                SERVICE_WIN32_OWN_PROCESS,
            ),
            (
                r#""C:\ProgramData\ZenClashService\zenclash-service.exe" run extra"#,
                "LocalSystem",
                "",
                "",
                SERVICE_WIN32_OWN_PROCESS,
            ),
            (
                r#""C:\ProgramData\ZenClashService\zenclash-service.exe" --install"#,
                "LocalSystem",
                "",
                "",
                SERVICE_WIN32_OWN_PROCESS,
            ),
            (
                r#""C:\ProgramData\ZenClashService\zenclash-service.exe""#,
                "LocalSystem",
                "",
                "",
                SERVICE_WIN32_OWN_PROCESS,
            ),
            (
                r"zenclash-service.exe run",
                "LocalSystem",
                "",
                "",
                SERVICE_WIN32_OWN_PROCESS,
            ),
            (
                r#""C:\ProgramData\other\..\ZenClashService\zenclash-service.exe" run"#,
                "LocalSystem",
                "",
                "",
                SERVICE_WIN32_OWN_PROCESS,
            ),
        ] {
            let calls = Cell::new(0);
            let result = configuration(command, account, group, dependency, kind)
                .require_owned(Path::new(EXPECTED))
                .map(|()| calls.set(calls.get() + 1));
            assert!(
                result.is_err(),
                "foreign configuration accepted: {command} {account}"
            );
            assert_eq!(calls.get(), 0);
        }
    }

    #[test]
    fn registration_native_strings_are_bounded_before_dereferencing() {
        for position in [
            0,
            1,
            std::mem::size_of::<QUERY_SERVICE_CONFIGW>() - 2,
            1023,
            1024,
            2048,
        ] {
            let mut buffer =
                configuration(COMMAND, "LocalSystem", "", "", SERVICE_WIN32_OWN_PROCESS);
            let address = (buffer.words.as_ptr() as usize + position) as *mut u16;
            unsafe {
                (*buffer.words.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>()).lpBinaryPathName =
                    address
            };
            assert!(buffer.require_owned(Path::new(EXPECTED)).is_err());
        }
        let mut buffer = configuration(COMMAND, "LocalSystem", "", "", SERVICE_WIN32_OWN_PROCESS);
        let last = unsafe {
            buffer
                .words
                .as_mut_ptr()
                .cast::<u8>()
                .add(1022)
                .cast::<u16>()
        };
        unsafe {
            *last = u16::from(b'x');
            (*buffer.words.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>()).lpBinaryPathName = last;
        }
        assert!(buffer.require_owned(Path::new(EXPECTED)).is_err());
    }

    #[test]
    fn registration_oversized_configuration_stops_before_allocation_or_second_query() {
        let calls = Cell::new(0);
        let result = query_configuration_with(
            |_, _, needed| {
                calls.set(calls.get() + 1);
                *needed = (MAX_CONFIGURATION_BYTES + 1) as u32;
                Err(io::Error::from_raw_os_error(
                    ERROR_INSUFFICIENT_BUFFER as i32,
                ))
            },
            |_| panic!("over-budget configuration must not reach review"),
        );
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn registration_native_query_error_is_not_absence_or_approval() {
        let result = query_configuration_with(
            |_, _, _| Err(io::Error::from_raw_os_error(5)),
            |_| panic!("failed query must not reach review"),
        );
        assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
    }

    #[test]
    fn registration_successful_query_ignores_unspecified_needed_output() {
        let calls = Cell::new(0);
        query_configuration_with(
            |target, length, needed| {
                calls.set(calls.get() + 1);
                if target.is_null() {
                    *needed = 1024;
                    return Err(io::Error::from_raw_os_error(
                        ERROR_INSUFFICIENT_BUFFER as i32,
                    ));
                }
                assert_eq!(length, 1024);
                let fixture =
                    configuration(COMMAND, "LocalSystem", "", "", SERVICE_WIN32_OWN_PROCESS);
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        fixture.words.as_ptr().cast::<u8>(),
                        target.cast::<u8>(),
                        1024,
                    )
                };
                let header = unsafe { &mut *target };
                for pointer in [
                    &mut header.lpBinaryPathName,
                    &mut header.lpServiceStartName,
                    &mut header.lpLoadOrderGroup,
                    &mut header.lpDependencies,
                ] {
                    *pointer = (target as usize
                        + (*pointer as usize - fixture.words.as_ptr() as usize))
                        as *mut u16;
                }
                *needed = 0;
                Ok(())
            },
            |buffer| buffer.require_owned(Path::new(EXPECTED)),
        )
        .unwrap();
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn registration_owned_configuration_does_not_require_a_present_executable() {
        configuration(
            COMMAND,
            r"NT AUTHORITY\SYSTEM",
            "",
            "",
            SERVICE_WIN32_OWN_PROCESS,
        )
        .require_owned(Path::new(EXPECTED))
        .unwrap();
    }

    #[test]
    fn registration_fixed_run_command_allows_one_operation() {
        let executable = Path::new(r"C:\ProgramData\ZenClashService\zenclash-service.exe");
        let calls = Cell::new(0);
        let result = require_fixed_executable(
            std::ffi::OsStr::new(r#""C:\ProgramData\ZenClashService\zenclash-service.exe" run"#),
            executable,
        )
        .map(|()| calls.set(calls.get() + 1));
        assert!(
            result.is_ok(),
            "fixed registered command rejected: {result:?}"
        );
        assert_eq!(calls.get(), 1);
    }
}
