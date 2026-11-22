// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Platform-native on-demand service activation (macOS launchd XPC and Windows SCM pipe triggers).

#[cfg(all(target_os = "macos", any(not(feature = "test"), test)))]
pub(crate) mod macos;

#[cfg(windows)]
pub(crate) mod windows;

#[cfg(all(windows, feature = "client", any(not(feature = "test"), test)))]
pub(crate) mod windows_identity;
