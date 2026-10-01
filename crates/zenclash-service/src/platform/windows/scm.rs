use std::{io, ptr};
#[cfg(feature = "server")]
use std::{
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(feature = "server")]
use tokio::sync::watch;
use windows_sys::Win32::{Foundation::ERROR_SERVICE_DOES_NOT_EXIST, System::Services::*};
#[cfg(feature = "server")]
use windows_sys::Win32::{
    Foundation::{
        ERROR_INVALID_PARAMETER, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_NOT_ACTIVE,
        WAIT_OBJECT_0,
    },
    Storage::FileSystem::DELETE,
    System::Threading::WaitForSingleObject,
};

use super::{SERVICE_NAME, wide};
#[cfg(feature = "server")]
use super::{
    denied,
    security::{service_root, validate_protected_path},
};

struct ServiceHandle(SC_HANDLE);
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe { CloseServiceHandle(self.0) };
    }
}

fn manager(access: u32) -> io::Result<ServiceHandle> {
    let handle = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), access) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(ServiceHandle(handle))
}

fn service(manager: &ServiceHandle, access: u32) -> io::Result<ServiceHandle> {
    let name = wide(SERVICE_NAME)?;
    let handle = unsafe { OpenServiceW(manager.0, name.as_ptr(), access) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(ServiceHandle(handle))
}

#[cfg(feature = "server")]
pub(crate) fn service_state() -> io::Result<Option<u32>> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let handle = match service(&manager, SERVICE_QUERY_STATUS) {
        Ok(service) => service,
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    Ok(Some(query(&handle)?.dwCurrentState))
}

pub(super) fn service_pid() -> io::Result<Option<u32>> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let service = match service(&manager, SERVICE_QUERY_STATUS) {
        Ok(service) => service,
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let status = query(&service)?;
    Ok(
        (status.dwCurrentState == SERVICE_RUNNING && status.dwProcessId != 0)
            .then_some(status.dwProcessId),
    )
}

fn query(service: &ServiceHandle) -> io::Result<SERVICE_STATUS_PROCESS> {
    let mut status: SERVICE_STATUS_PROCESS = unsafe { std::mem::zeroed() };
    let mut needed = 0;
    if unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of_val(&status) as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(status)
}

#[cfg(feature = "server")]
pub(crate) fn register_service() -> io::Result<()> {
    let root = service_root()?;
    validate_protected_path(&root, true)?;
    let executable = root.join("zenclash-service.exe");
    validate_protected_path(&executable, false)?;
    let binary = wide(format!("\"{}\" run", executable.display()))?;
    let name = wide(SERVICE_NAME)?;
    let display = wide("ZenClash Runtime Service")?;
    let manager = manager(SC_MANAGER_CREATE_SERVICE | SC_MANAGER_CONNECT)?;
    let existing = service(&manager, SERVICE_CHANGE_CONFIG | SERVICE_QUERY_STATUS);
    match existing {
        Ok(existing) => {
            if query(&existing)?.dwCurrentState != SERVICE_STOPPED {
                return Err(denied("stop service before replacement"));
            }
            let system = wide("LocalSystem")?;
            if unsafe {
                ChangeServiceConfigW(
                    existing.0,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    binary.as_ptr(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null(),
                    system.as_ptr(),
                    ptr::null(),
                    display.as_ptr(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {
            let handle = unsafe {
                CreateServiceW(
                    manager.0,
                    name.as_ptr(),
                    display.as_ptr(),
                    SERVICE_QUERY_STATUS,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    binary.as_ptr(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                )
            };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            drop(ServiceHandle(handle));
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

#[cfg(feature = "server")]
pub(crate) fn start_service() -> io::Result<()> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let service = service(&manager, SERVICE_START | SERVICE_QUERY_STATUS)?;
    if unsafe { StartServiceW(service.0, 0, ptr::null()) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_SERVICE_ALREADY_RUNNING as i32) {
            return Err(error);
        }
    }
    wait_state(&service, SERVICE_RUNNING)
}

#[cfg(feature = "server")]
pub(crate) fn stop_service() -> io::Result<()> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let service = match service(&manager, SERVICE_STOP | SERVICE_QUERY_STATUS) {
        Ok(service) => service,
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let previous = query(&service)?;
    let process = if previous.dwProcessId != 0 {
        match super::identity::process_handle(previous.dwProcessId) {
            Ok(process) => Some(process),
            Err(error) if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) => None,
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_SERVICE_NOT_ACTIVE as i32) {
            return Err(error);
        }
    }
    wait_state(&service, SERVICE_STOPPED)?;
    // SCM STOPPED is reported by the callback before the hosting process exits.
    // Await the pinned process before replacing its image or approving a restart.
    if let Some(process) = process
        && unsafe { WaitForSingleObject(process.0, 10000) } != WAIT_OBJECT_0
    {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "service stopped but host process has not exited",
        ));
    }
    Ok(())
}

#[cfg(feature = "server")]
fn wait_state(service: &ServiceHandle, expected: u32) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = query(service)?;
        if status.dwCurrentState == expected {
            return Ok(());
        }
        if expected == SERVICE_RUNNING && status.dwCurrentState == SERVICE_STOPPED {
            return Err(io::Error::other("service exited before becoming ready"));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "service transition timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(feature = "server")]
pub(crate) fn unregister_service() -> io::Result<()> {
    stop_service()?;
    let manager = manager(SC_MANAGER_CONNECT)?;
    let service = match service(&manager, DELETE) {
        Ok(service) => service,
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if unsafe { DeleteService(service.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(feature = "server")]
type ServiceRunner = fn(watch::Receiver<bool>) -> io::Result<()>;
#[cfg(feature = "server")]
static RUNNER: OnceLock<ServiceRunner> = OnceLock::new();
#[cfg(feature = "server")]
static STOP: OnceLock<watch::Sender<bool>> = OnceLock::new();
#[cfg(feature = "server")]
static STATUS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "server")]
pub(crate) fn dispatch_service(runner: ServiceRunner) -> io::Result<()> {
    RUNNER
        .set(runner)
        .map_err(|_| io::Error::other("service dispatcher already started"))?;
    let mut name = wide(SERVICE_NAME)?;
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(feature = "server")]
unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    let Ok(name) = wide(SERVICE_NAME) else {
        return;
    };
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control_handler), ptr::null_mut())
    };
    if handle.is_null() {
        return;
    }
    STATUS.store(handle as usize, Ordering::Release);
    let (sender, receiver) = watch::channel(false);
    if STOP.set(sender).is_err() {
        report(SERVICE_STOPPED, 1);
        return;
    }
    report(SERVICE_START_PENDING, 0);
    let result = RUNNER
        .get()
        .ok_or_else(|| io::Error::other("service runner missing"))
        .and_then(|run| run(receiver));
    report(SERVICE_STOPPED, if result.is_ok() { 0 } else { 1 });
}

#[cfg(feature = "server")]
pub(crate) fn report_ready() {
    report(SERVICE_RUNNING, 0);
}

#[cfg(feature = "server")]
unsafe extern "system" fn control_handler(
    control: u32,
    _: u32,
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
) -> u32 {
    if matches!(control, SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN) {
        report(SERVICE_STOP_PENDING, 0);
        if let Some(stop) = STOP.get() {
            let _ = stop.send(true);
        }
    }
    0
}

#[cfg(feature = "server")]
fn report(state: u32, error: u32) {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: error,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: u32::from(matches!(
            state,
            SERVICE_START_PENDING | SERVICE_STOP_PENDING
        )),
        dwWaitHint: if matches!(state, SERVICE_START_PENDING | SERVICE_STOP_PENDING) {
            30000
        } else {
            0
        },
    };
    let handle = STATUS.load(Ordering::Acquire) as SERVICE_STATUS_HANDLE;
    if !handle.is_null() {
        unsafe { SetServiceStatus(handle, &status) };
    }
}
