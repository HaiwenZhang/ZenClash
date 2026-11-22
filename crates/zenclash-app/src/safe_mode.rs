//! Runtime Safe Mode policy isolating development and diagnostic sessions from
//! host-level network, service, proxy, and autostart side effects.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

/// Environment variable that activates Safe Mode when set to a truthy value
/// (`1`, `true`, `yes`, or `on`, case-insensitive).
pub const SAFE_MODE_ENV_VAR: &str = "ZENCLASH_SAFE_MODE";

/// CLI flag that activates Safe Mode for the current process.
pub const SAFE_MODE_CLI_FLAG: &str = "--safe-mode";

/// Isolated runtime policy that prevents host-level side effects so development
/// and diagnostic sessions can coexist safely with a production `ZenClash`
/// instance.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SafeMode {
    enabled: bool,
}

impl SafeMode {
    /// Returns a disabled Safe Mode policy (normal production behavior).
    #[must_use]
    pub const fn disabled() -> Self {
        Self { enabled: false }
    }

    /// Returns an enabled Safe Mode policy with host side effects blocked.
    #[must_use]
    pub const fn enabled() -> Self {
        Self { enabled: true }
    }

    /// Detects Safe Mode from process CLI arguments (`--safe-mode`) and the
    /// `ZENCLASH_SAFE_MODE` environment variable.
    #[must_use]
    pub fn detect() -> Self {
        let env_value = std::env::var_os(SAFE_MODE_ENV_VAR);
        Self::from_inputs(std::env::args_os().skip(1), env_value.as_deref())
    }

    /// Evaluates Safe Mode from explicit CLI arguments and an optional
    /// environment variable value.
    #[must_use]
    pub fn from_inputs<I, S>(args: I, env_value: Option<&OsStr>) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let cli_enabled = args
            .into_iter()
            .any(|arg| arg.as_ref() == OsStr::new(SAFE_MODE_CLI_FLAG));
        let env_enabled = env_value.is_some_and(is_truthy_os_str);
        Self {
            enabled: cli_enabled || env_enabled,
        }
    }

    /// Returns `true` when Safe Mode is active.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    /// Derives the application data directory for this policy.
    ///
    /// When Safe Mode is active, persistent state is isolated under a dedicated
    /// `safe-mode` subdirectory beneath `base_data_root`.
    #[must_use]
    pub fn data_root(self, base_data_root: &Path) -> PathBuf {
        if self.enabled {
            base_data_root.join("safe-mode")
        } else {
            base_data_root.to_path_buf()
        }
    }

    /// Derives the single-instance lock file path for this policy so a Safe Mode
    /// process does not collide with a running production instance.
    #[must_use]
    pub fn instance_lock_path(self, base_data_root: &Path) -> PathBuf {
        self.data_root(base_data_root).join("instance.lock")
    }

    /// Returns whether modifying OS system proxy settings is permitted.
    #[must_use]
    pub const fn allows_system_proxy(self) -> bool {
        !self.enabled
    }

    /// Returns whether interacting with the privileged TUN service is permitted.
    #[must_use]
    pub const fn allows_privileged_service(self) -> bool {
        !self.enabled
    }

    /// Returns whether enabling TUN capture is permitted.
    #[must_use]
    pub const fn allows_tun(self) -> bool {
        !self.enabled
    }

    /// Returns whether modifying OS login autostart entries is permitted.
    #[must_use]
    pub const fn allows_autostart(self) -> bool {
        !self.enabled
    }

    /// Returns whether modifying OS system DNS settings is permitted.
    #[must_use]
    pub const fn allows_system_dns(self) -> bool {
        !self.enabled
    }

    /// Returns whether binding default production proxy/controller ports is permitted.
    #[must_use]
    pub const fn allows_default_ports(self) -> bool {
        !self.enabled
    }

    /// Guards a host-mutating side effect, short-circuiting with `Ok(None)` when
    /// Safe Mode is active.
    ///
    /// # Errors
    ///
    /// Returns any error produced by `action` when Safe Mode is disabled.
    pub fn guard_host_side_effect<T, E>(
        self,
        action_name: &str,
        action: impl FnOnce() -> Result<T, E>,
    ) -> Result<Option<T>, E> {
        if self.enabled {
            tracing::info!(
                action = action_name,
                "skipped host side effect because Safe Mode is active"
            );
            return Ok(None);
        }
        action().map(Some)
    }
}

fn is_truthy_os_str(value: &OsStr) -> bool {
    let Some(text) = value.to_str() else {
        return false;
    };
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn safe_mode_is_disabled_by_default() {
        let mode = SafeMode::from_inputs(["--other"], None);
        assert!(!mode.is_enabled());
        assert!(mode.allows_system_proxy());
        assert!(mode.allows_privileged_service());
        assert!(mode.allows_tun());
        assert!(mode.allows_autostart());
        assert!(mode.allows_system_dns());
        assert!(mode.allows_default_ports());
    }

    #[test]
    fn safe_mode_activates_from_cli_flag() {
        let mode = SafeMode::from_inputs(["--continue-local", SAFE_MODE_CLI_FLAG], None);
        assert!(mode.is_enabled());
        assert!(!mode.allows_system_proxy());
        assert!(!mode.allows_privileged_service());
        assert!(!mode.allows_tun());
        assert!(!mode.allows_autostart());
        assert!(!mode.allows_system_dns());
        assert!(!mode.allows_default_ports());
    }

    #[test]
    fn safe_mode_activates_only_for_truthy_environment_values() {
        for truthy in ["1", "true", "TRUE", " yes ", "On"] {
            let mode = SafeMode::from_inputs(std::iter::empty::<&str>(), Some(OsStr::new(truthy)));
            assert!(mode.is_enabled(), "expected {truthy:?} to enable Safe Mode");
        }
        for falsy in ["", "0", "false", "no", "off", "2"] {
            let mode = SafeMode::from_inputs(std::iter::empty::<&str>(), Some(OsStr::new(falsy)));
            assert!(
                !mode.is_enabled(),
                "expected {falsy:?} to keep Safe Mode disabled"
            );
        }
    }

    #[test]
    fn safe_mode_isolates_data_root_and_instance_lock_paths() {
        let base = Path::new("/data/zenclash");
        assert_eq!(SafeMode::disabled().data_root(base), base);
        assert_eq!(
            SafeMode::disabled().instance_lock_path(base),
            base.join("instance.lock")
        );

        let safe = SafeMode::enabled();
        assert_eq!(safe.data_root(base), base.join("safe-mode"));
        assert_eq!(
            safe.instance_lock_path(base),
            base.join("safe-mode").join("instance.lock")
        );
    }

    #[test]
    fn safe_mode_guard_short_circuits_host_side_effects() {
        let called = Cell::new(false);
        let skipped = SafeMode::enabled()
            .guard_host_side_effect("set_system_proxy", || {
                called.set(true);
                Ok::<_, String>(42)
            })
            .unwrap();
        assert_eq!(skipped, None);
        assert!(!called.get());

        let executed = SafeMode::disabled()
            .guard_host_side_effect("set_system_proxy", || {
                called.set(true);
                Ok::<_, String>(42)
            })
            .unwrap();
        assert_eq!(executed, Some(42));
        assert!(called.get());
    }
}
