//! Native reporting for failures raised before the application window exists.

/// Presents a platform-native error dialog when one is available.
///
/// This is reserved for bootstrap failures where no GPUI window can be shown.
/// Callers already print the message to stderr; this helper only adds a native
/// surface so GUI launches do not fail silently.
pub fn show(message: &str) {
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
use linux as platform;

#[cfg(any(target_os = "linux", test))]
mod linux {
    use std::{
        io,
        process::{Command, ExitStatus, Stdio},
    };

    #[cfg(target_os = "linux")]
    pub(super) fn show(message: &str) {
        show_with(message, |command| command.status());
    }

    pub(super) fn show_with(
        message: &str,
        mut run: impl FnMut(&mut Command) -> io::Result<ExitStatus>,
    ) {
        // Try the two common desktop dialogs, then a persistent notification.
        // Headless sessions have none of them and keep the stderr output.
        let escaped = message
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        // KDialog parses backslash escapes before passing text to Qt's rich
        // text label. Entities preserve literal backslashes through both steps.
        let kde_message = format!(
            "<html>{}</html>",
            escaped.replace('\\', "&#92;").replace('\n', "<br>")
        );
        let candidates: [(&str, &[&str]); 3] = [
            (
                "zenity",
                &[
                    "--error",
                    "--no-markup",
                    "--title",
                    "ZenClash",
                    "--text",
                    message,
                ],
            ),
            ("kdialog", &["--title", "ZenClash", "--error", &kde_message]),
            ("notify-send", &["--urgency=critical", "ZenClash", &escaped]),
        ];
        for (program, arguments) in candidates {
            let mut command = Command::new(program);
            command
                .args(arguments)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if program == "zenity" {
                // A dismissed dialog has already reported the error. Normalize
                // Zenity's configurable response codes without changing startup
                // failures (for example, an unavailable display) into success.
                command
                    .env("ZENITY_OK", "0")
                    .env("ZENITY_ESC", "0")
                    .env("ZENITY_CANCEL", "0")
                    .env("ZENITY_ERROR", "255");
            }
            if run(&mut command).is_ok_and(|status| {
                // KDialog returns 2 when its displayed error dialog is dismissed.
                status.success() || (program == "kdialog" && status.code() == Some(2))
            }) {
                return;
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod platform {
    pub(super) fn show(_message: &str) {}
}

#[cfg(test)]
mod tests;
