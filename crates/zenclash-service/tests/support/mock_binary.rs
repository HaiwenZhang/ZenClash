#![cfg(feature = "test")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let ipc_path = args
        .windows(2)
        .find(|args| args[0] == "-ext-ctl-unix" || args[0] == "-ext-ctl-pipe")
        .map(|args| args[1].clone());

    #[cfg(unix)]
    let _listener = ipc_path.and_then(|path| std::os::unix::net::UnixListener::bind(path).ok());

    #[cfg(windows)]
    let _pipe = ipc_path.and_then(create_test_pipe);

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        println!("Still running...");
    }
}

#[cfg(windows)]
fn create_test_pipe(path: String) -> Option<TestPipe> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    let mut wide: Vec<u16> = std::ffi::OsStr::new(&path).encode_wide().collect();
    wide.push(0);
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    (handle != INVALID_HANDLE_VALUE).then_some(TestPipe(handle))
}

#[cfg(windows)]
struct TestPipe(*mut std::ffi::c_void);

#[cfg(windows)]
impl Drop for TestPipe {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}
