//! Native reporting for failures raised before the application window exists.

/// Presents a platform-native error dialog when one is available.
///
/// This is reserved for bootstrap failures where no GPUI window can be shown.
/// Callers already print the message to stderr; this helper only adds a native
/// surface so GUI launches do not fail silently.
pub(crate) fn show(message: &str) {
    platform::show(message);
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2_app_kit::{NSAlert, NSAlertStyle, NSApplication};
    use objc2_foundation::{MainThreadMarker, NSString};

    pub(super) fn show(message: &str) {
        // Startup failures happen before GPUI creates its event loop, so this
        // is still the application's main thread.
        let Some(marker) = MainThreadMarker::new() else {
            return;
        };
        let application = NSApplication::sharedApplication(marker);
        application.activate();
        let alert = NSAlert::new(marker);
        alert.setMessageText(&NSString::from_str("ZenClash"));
        alert.setInformativeText(&NSString::from_str(message));
        alert.setAlertStyle(NSAlertStyle::Critical);
        let _response = alert.runModal();
    }
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    pub(super) fn show(message: &str) {
        let title: Vec<u16> = "ZenClash".encode_utf16().chain(Some(0)).collect();
        let body: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
        // SAFETY: Both strings are NUL-terminated and remain alive for the whole
        // call. A null owner window is valid and creates a modal top-level dialog.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                body.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::process::{Command, Stdio};

    pub(super) fn show(message: &str) {
        // Try the two common desktop dialogs, then a persistent notification.
        // Headless sessions have none of them and keep the stderr output.
        let candidates: [(&str, &[&str]); 3] = [
            (
                "zenity",
                &["--error", "--title", "ZenClash", "--text", message],
            ),
            ("kdialog", &["--title", "ZenClash", "--error", message]),
            ("notify-send", &["--urgency=critical", "ZenClash", message]),
        ];
        for (program, arguments) in candidates {
            let available = Command::new(program)
                .args(arguments)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok();
            if available {
                return;
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod platform {
    pub(super) fn show(_message: &str) {}
}
