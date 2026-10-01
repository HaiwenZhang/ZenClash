use std::{
    fs::File,
    io,
    os::windows::{ffi::OsStringExt, io::FromRawHandle},
    path::{Path, PathBuf},
    ptr,
};

use windows_sys::Win32::{
    Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::SECURITY_ATTRIBUTES,
    System::{
        JobObjects::TerminateJobObject,
        Pipes::CreatePipe,
        SystemInformation::GetSystemWindowsDirectoryW,
        Threading::{
            CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
            InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
            STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

use super::{ChildGuard, Handle, install::quote_argument, security::validate_protected_path, wide};

/// A child created atomically inside its non-inheritable close-on-crash Job.
pub(crate) struct NativeChild {
    process: Handle,
    job: ChildGuard,
    pid: u32,
    pub(crate) stdout: Option<File>,
    pub(crate) stderr: Option<File>,
}

impl NativeChild {
    pub(crate) fn spawn(executable: &Path, config: &Path, home: &Path) -> io::Result<Self> {
        Self::spawn_mode(executable, config, home, LaunchMode::Run)
    }

    pub(crate) fn spawn_validation(
        executable: &Path,
        config: &Path,
        home: &Path,
    ) -> io::Result<Self> {
        Self::spawn_mode(executable, config, home, LaunchMode::Validate)
    }

    fn spawn_mode(
        executable: &Path,
        config: &Path,
        home: &Path,
        mode: LaunchMode,
    ) -> io::Result<Self> {
        validate_protected_path(executable, false)?;
        validate_protected_path(config, false)?;
        validate_protected_path(home, true)?;
        validate_protected_path(session_directory(home)?, true)?;
        validate_protected_path(&session_directory(home)?.join("assets"), true)?;
        let command = command_line(executable, config, home, mode)?;
        launch(executable, &command, home)
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    pub(crate) fn try_wait(&self) -> io::Result<Option<i32>> {
        match unsafe { WaitForSingleObject(self.process.0, 0) } {
            WAIT_OBJECT_0 => self.exit_code().map(Some),
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }

    pub(crate) fn kill(&self) -> io::Result<()> {
        // Terminate the entire job rather than leaving kernel descendants alive.
        if unsafe { TerminateJobObject(self.job.0.0, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(crate) fn wait(&self) -> io::Result<i32> {
        match unsafe { WaitForSingleObject(self.process.0, 5000) } {
            WAIT_OBJECT_0 => self.exit_code(),
            WAIT_TIMEOUT => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native child has not exited within five seconds",
            )),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected native process wait result",
            )),
        }
    }

    fn exit_code(&self) -> io::Result<i32> {
        let mut code = 0;
        if unsafe { GetExitCodeProcess(self.process.0, &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(code as i32)
    }
}

#[derive(Clone, Copy)]
enum LaunchMode {
    Run,
    Validate,
}

fn command_line(
    executable: &Path,
    config: &Path,
    home: &Path,
    mode: LaunchMode,
) -> io::Result<String> {
    let validation = match mode {
        LaunchMode::Run => "",
        LaunchMode::Validate => " -t",
    };
    Ok(format!(
        "{}{validation} -d {} -f {}",
        quote_argument(executable.as_os_str())?,
        quote_argument(home.as_os_str())?,
        quote_argument(config.as_os_str())?
    ))
}

fn system_windows_directory() -> io::Result<PathBuf> {
    let mut text = vec![0u16; 32768];
    let len = unsafe { GetSystemWindowsDirectoryW(text.as_mut_ptr(), text.len() as u32) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    if len as usize >= text.len() {
        return Err(io::Error::other(
            "Windows directory exceeds native path limit",
        ));
    }
    text.truncate(len as usize);
    Ok(std::ffi::OsString::from_wide(&text).into())
}

fn clean_environment(home: &Path) -> io::Result<Vec<u16>> {
    // The elevated maintenance helper may inherit caller-controlled variables.
    // Build an allowlist from the OS directory API and the protected session
    // directory, never from the parent's PATH, profile, proxy or runtime flags.
    let windows = system_windows_directory()?;
    let system = windows.join("System32");
    let assets = session_directory(home)?.join("assets");
    let entries = [
        ("APPDATA", home.as_os_str()),
        ("HOME", home.as_os_str()),
        // Mihomo's default pipe descriptor grants Builtin Users generic write,
        // including creating another server instance. Its controller is private
        // to this SYSTEM service, so override that upstream default explicitly.
        (
            "LISTEN_NAMEDPIPE_SDDL",
            std::ffi::OsStr::new("D:P(A;;GA;;;SY)"),
        ),
        ("LOCALAPPDATA", home.as_os_str()),
        ("PATH", system.as_os_str()),
        ("SAFE_PATHS", assets.as_os_str()),
        ("SYSTEMROOT", windows.as_os_str()),
        ("TEMP", home.as_os_str()),
        ("TMP", home.as_os_str()),
        ("USERPROFILE", home.as_os_str()),
        ("WINDIR", windows.as_os_str()),
    ];
    let mut block = Vec::new();
    // Names are uppercase and sorted as required by CreateProcessW.
    for (name, value) in entries {
        let mut entry = std::ffi::OsString::from(name);
        entry.push("=");
        entry.push(value);
        block.extend(wide(entry)?);
    }
    block.push(0);
    Ok(block)
}

fn session_directory(home: &Path) -> io::Result<&Path> {
    home.parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "session directory missing"))
}

impl Drop for NativeChild {
    fn drop(&mut self) {
        let _ = self.kill();
        // A bounded reap before handle destruction; Job closure remains the final
        // backstop if shutdown occurs while the child is still terminating.
        unsafe { WaitForSingleObject(self.process.0, 5000) };
    }
}

struct Attributes {
    storage: Vec<usize>,
}

impl Attributes {
    fn new() -> io::Result<Self> {
        let mut bytes = 0;
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 2, 0, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
        if unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 2, 0, &mut bytes)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { storage })
    }

    fn raw(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }

    fn set(&mut self, attribute: u32, handles: &[HANDLE]) -> io::Result<()> {
        if unsafe {
            UpdateProcThreadAttribute(
                self.raw(),
                0,
                attribute as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(handles),
                ptr::null_mut(),
                ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.raw()) };
    }
}

fn pipe() -> io::Result<(Handle, Handle)> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: 1,
    };
    let mut read = ptr::null_mut();
    let mut write = ptr::null_mut();
    if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let read = Handle(read);
    let write = Handle(write);
    Ok((read, write))
}

fn no_inherit(handle: &Handle) -> io::Result<()> {
    if unsafe { SetHandleInformation(handle.0, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn into_file(handle: Handle) -> File {
    let raw = handle.0;
    std::mem::forget(handle);
    unsafe { File::from_raw_handle(raw.cast()) }
}

fn launch(executable: &Path, command: &str, home: &Path) -> io::Result<NativeChild> {
    let environment = clean_environment(home)?;
    let job = ChildGuard::new()?;
    let (input_read, input_write) = pipe()?;
    let (output_read, output_write) = pipe()?;
    let (error_read, error_write) = pipe()?;
    no_inherit(&input_write)?;
    no_inherit(&output_read)?;
    no_inherit(&error_read)?;
    let handles = [input_read.0, output_write.0, error_write.0];
    let jobs = [job.0.0];
    let mut attributes = Attributes::new()?;
    attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = input_read.0;
    startup.StartupInfo.hStdOutput = output_write.0;
    startup.StartupInfo.hStdError = error_write.0;
    startup.lpAttributeList = attributes.raw();
    let executable = wide(executable)?;
    let mut command = wide(command)?;
    let home = wide(home)?;
    let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // The Job and explicit pipe handle lists are applied during process creation,
    // before the child can execute. No post-spawn assignment race exists.
    if unsafe {
        CreateProcessW(
            executable.as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            home.as_ptr(),
            &startup.StartupInfo,
            &mut information,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let process = Handle(information.hProcess);
    drop(Handle(information.hThread));
    drop(input_read);
    drop(input_write);
    drop(output_write);
    drop(error_write);
    Ok(NativeChild {
        process,
        job,
        pid: information.dwProcessId,
        stdout: Some(into_file(output_read)),
        stderr: Some(into_file(error_read)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle},
        System::{JobObjects::IsProcessInJob, Threading::GetCurrentProcess},
    };

    fn system_cmd() -> std::path::PathBuf {
        system_windows_directory()
            .unwrap()
            .join("System32")
            .join("cmd.exe")
    }

    #[tokio::test]
    async fn system_only_controller_descriptor_rejects_other_users_and_pipe_instances() {
        use super::super::{identity::current_identity, security::Descriptor};
        use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

        let descriptor = Descriptor::from_sddl("D:P(A;;GA;;;SY)").unwrap();
        let mut attributes = descriptor.attributes();
        let name = format!(
            r"\\.\pipe\ZenClash.SystemOnly.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let server = unsafe {
            ServerOptions::new()
                .first_pipe_instance(true)
                .create_with_security_attributes_raw(
                    &name,
                    (&mut attributes as *mut windows_sys::Win32::Security::SECURITY_ATTRIBUTES)
                        .cast(),
                )
        }
        .unwrap();
        let is_system = current_identity().unwrap().user() == "S-1-5-18";
        let client = ClientOptions::new().open(&name);
        let second_instance = ServerOptions::new().create(&name);
        assert_eq!(client.is_ok(), is_system);
        assert_eq!(second_instance.is_ok(), is_system);
        drop(server);
    }

    #[test]
    fn validation_mode_delivers_fixed_arguments_and_a_clean_environment_to_a_native_fixture() {
        // This ordinary-user fixture checks Windows process argv/environment,
        // not Mihomo configuration semantics or privileged installation.
        let home = std::env::temp_dir().join(format!(
            "zenclash-native-argv-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&home).unwrap();
        let source = home.join("fixture.rs");
        let executable = home.join("fixture.exe");
        let config = home.join("config with spaces.yaml");
        std::fs::write(
            &source,
            r#"fn main() {
                for arg in std::env::args().skip(1) { println!("ARG={arg}"); }
                let mut env: Vec<_> = std::env::vars().collect();
                env.sort();
                for (key, value) in env { println!("ENV={key}={value}"); }
            }"#,
        )
        .unwrap();
        let compiler = std::process::Command::new("rustc")
            .args(["--edition=2024", "--crate-name", "zenclash_native_fixture"])
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(compiler.status.success(), "{compiler:?}");
        let command = command_line(&executable, &config, &home, LaunchMode::Validate).unwrap();
        let mut child = launch(&executable, &command, &home).unwrap();
        let exit_code = child.wait().unwrap();
        let mut output = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .unwrap();
        assert_eq!(exit_code, 0);
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(
            &lines[..5],
            [
                "ARG=-t",
                "ARG=-d",
                &format!("ARG={}", home.display()),
                "ARG=-f",
                &format!("ARG={}", config.display()),
            ]
        );
        let keys: Vec<_> = lines[5..]
            .iter()
            .map(|line| {
                line.strip_prefix("ENV=")
                    .unwrap()
                    .split_once('=')
                    .unwrap()
                    .0
            })
            .collect();
        assert_eq!(
            keys,
            [
                "APPDATA",
                "HOME",
                "LISTEN_NAMEDPIPE_SDDL",
                "LOCALAPPDATA",
                "PATH",
                "SAFE_PATHS",
                "SYSTEMROOT",
                "TEMP",
                "TMP",
                "USERPROFILE",
                "WINDIR",
            ]
        );
        let pipe_descriptor = lines
            .iter()
            .find_map(|line| line.strip_prefix("ENV=LISTEN_NAMEDPIPE_SDDL="))
            .unwrap();
        assert_eq!(pipe_descriptor, "D:P(A;;GA;;;SY)");
        assert!(!pipe_descriptor.contains(";;;BU"));
        for name in [
            "APPDATA",
            "HOME",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
            "USERPROFILE",
        ] {
            assert!(lines.contains(&format!("ENV={name}={}", home.display()).as_str()));
        }
        let windows = system_windows_directory().unwrap();
        assert!(
            lines.contains(
                &format!(
                    "ENV=SAFE_PATHS={}",
                    home.parent().unwrap().join("assets").display()
                )
                .as_str()
            )
        );
        assert!(
            lines.contains(&format!("ENV=PATH={}", windows.join("System32").display()).as_str())
        );
        assert!(lines.contains(&format!("ENV=SYSTEMROOT={}", windows.display()).as_str()));
        assert!(lines.contains(&format!("ENV=WINDIR={}", windows.display()).as_str()));
        drop(child);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn validation_rejects_an_unprotected_executable_before_launch() {
        let executable = std::env::current_exe().unwrap();
        let error =
            match NativeChild::spawn_validation(&executable, &executable, &std::env::temp_dir()) {
                Ok(_) => panic!("unprotected executable was accepted"),
                Err(error) => error,
            };
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn native_child_is_already_in_its_job_when_spawn_returns_and_streams_are_captured() {
        let cmd = system_cmd();
        let mut child = launch(
            &cmd,
            &format!("{} /c echo hello", quote_argument(cmd.as_os_str()).unwrap()),
            &std::env::temp_dir(),
        )
        .unwrap();
        let mut in_job = 0;
        assert_ne!(
            unsafe { IsProcessInJob(child.process.0, child.job.0.0, &mut in_job) },
            0
        );
        assert_ne!(in_job, 0);
        let exit_code = child.wait().unwrap();
        let mut output = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .unwrap();
        let mut error = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut error)
            .unwrap();
        assert_eq!(exit_code, 0, "stdout={output:?}, stderr={error:?}");
        assert_eq!(output.trim(), "hello");
    }

    #[test]
    fn waiting_times_out_without_claiming_a_live_child_has_exited() {
        let cmd = system_cmd();
        let child = launch(
            &cmd,
            &format!(
                "{} /c ping -n 15 127.0.0.1 >nul",
                quote_argument(cmd.as_os_str()).unwrap()
            ),
            &std::env::temp_dir(),
        )
        .unwrap();
        let started = std::time::Instant::now();
        let result = child.wait();
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        assert_eq!(child.wait().unwrap(), 1);
    }

    #[test]
    fn dropping_the_native_child_stops_a_live_process() {
        let cmd = system_cmd();
        let child = launch(
            &cmd,
            &format!(
                "{} /c ping -n 30 127.0.0.1 >nul",
                quote_argument(cmd.as_os_str()).unwrap()
            ),
            &std::env::temp_dir(),
        )
        .unwrap();
        let mut observer = ptr::null_mut();
        let current = unsafe { GetCurrentProcess() };
        assert_ne!(
            unsafe {
                DuplicateHandle(
                    current,
                    child.process.0,
                    current,
                    &mut observer,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            },
            0
        );
        assert!(child.try_wait().unwrap().is_none());
        drop(child);
        let result = unsafe { WaitForSingleObject(observer, 5000) };
        unsafe { CloseHandle(observer) };
        assert_eq!(result, WAIT_OBJECT_0);
    }
}
