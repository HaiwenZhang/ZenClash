// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
use anyhow::{Context as _, Result, bail};
use std::{
    os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
    path::Path,
    sync::mpsc,
    thread,
};
use windows_sys::Win32::{
    Foundation::{LocalFree, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        SECURITY_ATTRIBUTES,
    },
    System::Threading::{CreateMutexExW, ReleaseMutex, WaitForSingleObject},
};

#[cfg(not(feature = "test"))]
pub(super) const NAME: &str = r"Global\zenclash-service.core-execution";
#[cfg(feature = "test")]
pub(super) const NAME: &str = r"Global\zenclash-service.core-execution-test";

#[derive(Debug)]
pub(super) struct Reservation {
    release: Option<mpsc::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
    legacy: bool,
}

impl Reservation {
    pub(super) fn try_acquire(name: &str, path: &Path) -> Result<Option<Self>> {
        let name = name.to_owned();
        let path = path.to_owned();
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::channel();
        // Windows mutex ownership belongs to a thread, while a guard crosses Tokio threads.
        // Each attempt needs its own owner thread so same-process probes cannot acquire recursively.
        let worker = thread::Builder::new().name("core-execution-lock".into()).spawn(move || {
            let acquired = (|| -> Result<Option<(MutexOwner, Option<std::fs::File>)>> {
                let Some(owner) = MutexOwner::try_acquire(&name)? else { return Ok(None) };
                let legacy = match super::open_coordination_file(&path) {
                    Ok(file) => {
                        match file.try_lock() {
                            Ok(()) => {}
                            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                            Err(std::fs::TryLockError::Error(error)) => {
                                return Err(error).with_context(|| format!("could not reserve legacy core execution lock {path:?}"));
                            }
                        }
                        Some(file)
                    }
                    Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
                        && matches!(std::fs::symlink_metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound) => None,
                    Err(error) => return Err(error),
                };
                Ok(Some((owner, legacy)))
            })();
            match acquired {
                Ok(Some((owner, legacy))) => {
                    if ready_tx.send(Ok(Some(legacy.is_some()))).is_ok() {
                        let _ = release_rx.recv();
                    }
                    drop(legacy);
                    drop(owner);
                }
                Ok(None) => { let _ = ready_tx.send(Ok(None)); }
                Err(error) => { let _ = ready_tx.send(Err(error)); }
            }
        }).context("could not start core execution lock owner")?;
        let ready = ready_rx
            .recv()
            .context("core execution lock owner exited before reporting readiness");
        match ready.and_then(|result| result) {
            Ok(Some(legacy)) => Ok(Some(Self {
                release: Some(release_tx),
                worker: Some(worker),
                legacy,
            })),
            result => {
                drop(release_tx);
                let _ = worker.join();
                result.map(|_| None)
            }
        }
    }

    pub(super) fn has_legacy_lock(&self) -> bool {
        self.legacy
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct MutexOwner(OwnedHandle);

impl MutexOwner {
    fn try_acquire(name: &str) -> Result<Option<Self>> {
        // Explicit owner rights prevent the creator from implicitly receiving WRITE_DAC.
        let sddl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;RC;;;OW)(A;;0x00100001;;;AU)\0"
            .encode_utf16()
            .collect();
        let mut descriptor = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error()).context("could not create core execution mutex permissions");
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let raw = unsafe { CreateMutexExW(&attributes, wide.as_ptr(), 0, 0x00100001) };
        let error = std::io::Error::last_os_error();
        unsafe {
            LocalFree(descriptor);
        }
        if raw.is_null() {
            return Err(error).with_context(|| format!("could not open core execution mutex {name:?}"));
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
            // The caller checks for surviving cores while holding the reservation.
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Some(Self(handle))),
            WAIT_TIMEOUT => Ok(None),
            _ => bail!(
                "could not reserve core execution mutex {name:?}: {}",
                std::io::Error::last_os_error()
            ),
        }
    }
}

impl Drop for MutexOwner {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0.as_raw_handle());
        }
    }
}
