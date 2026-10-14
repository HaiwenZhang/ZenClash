use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::resources::{
    bundled_core_binary, find_core_binary, install_bundled_core, install_bundled_core_with_lease,
    install_bundled_mihomo_data, is_core_binary_candidate, managed_core_binary,
};
use crate::{
    CoreCapabilities, CoreConfigValidationError, CoreConfigValidator, CoreKind, MihomoEndpoint,
    MihomoError, MihomoResult, profiles::read_profile_bytes,
};

/// Resolved inputs used to launch the selected managed core implementation.
#[derive(Clone, Debug)]
pub struct CoreLaunchConfig {
    /// Runtime core selected explicitly by the user or caller.
    pub kind: CoreKind,
    /// Core executable selected from an override, bundle, workspace, or `PATH`.
    pub binary: PathBuf,
    /// Original YAML source; Local Mihomo freezes its bytes and passes them via stdin.
    pub config_file: PathBuf,
    /// Writable data directory passed to the selected core with `-d`.
    pub home_dir: PathBuf,
    /// Private IPC controller for Mihomo, or configured HTTP controller for other cores.
    pub endpoint: MihomoEndpoint,
    /// Optional isolated controller address passed with `-ext-ctl`.
    pub controller_override: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FileControllerConfig {
    #[serde(default, rename = "external-controller")]
    external_controller: String,
    #[serde(default)]
    secret: String,
}

impl CoreLaunchConfig {
    /// Builds a launch configuration with a private Mihomo IPC controller.
    ///
    /// # Errors
    ///
    /// Returns an error when the configuration cannot be read or parsed.
    pub fn new(
        binary: impl Into<PathBuf>,
        config_file: impl Into<PathBuf>,
        home_dir: impl Into<PathBuf>,
    ) -> MihomoResult<Self> {
        Self::for_kind(CoreKind::Mihomo, binary, config_file, home_dir)
    }

    /// Builds a launch configuration for one explicit runtime core.
    ///
    /// # Errors
    ///
    /// Returns an error when the configuration cannot be read or parsed.
    pub fn for_kind(
        kind: CoreKind,
        binary: impl Into<PathBuf>,
        config_file: impl Into<PathBuf>,
        home_dir: impl Into<PathBuf>,
    ) -> MihomoResult<Self> {
        let config_file = config_file.into();
        let home_dir = home_dir.into();
        let configured_endpoint = endpoint_from_config_file(&config_file)?;
        let endpoint = if kind == CoreKind::Mihomo {
            MihomoEndpoint::local_ipc(&home_dir)?
        } else {
            configured_endpoint
        };
        Ok(Self {
            kind,
            binary: binary.into(),
            config_file,
            home_dir,
            endpoint,
            controller_override: None,
        })
    }

    /// Overrides the selected core controller and the compatible [`crate::MihomoClient`].
    #[must_use]
    pub fn with_controller_override(mut self, controller: impl Into<String>) -> Self {
        let controller = controller.into();
        self.endpoint.controller.clone_from(&controller);
        self.controller_override = Some(controller);
        self
    }

    /// Overrides both the managed controller address and its authentication secret.
    #[must_use]
    pub fn with_controller_endpoint(mut self, endpoint: MihomoEndpoint) -> Self {
        self.controller_override = Some(endpoint.controller.clone());
        self.endpoint = endpoint;
        self
    }

    /// Discovers launch inputs from environment overrides, bundled resources,
    /// workspace fallbacks, and finally the process `PATH`.
    ///
    /// # Errors
    ///
    /// Returns an error if no executable is found or the selected YAML is invalid.
    pub fn discover(project_root: impl AsRef<Path>) -> MihomoResult<Self> {
        Self::discover_for_kind(project_root, CoreKind::Mihomo)
    }

    /// Discovers launch inputs for an explicit runtime core.
    ///
    ///
    /// # Errors
    ///
    /// Returns an error if the selected executable is absent or invalid, or
    /// when the selected YAML cannot be read.
    pub fn discover_for_kind(project_root: impl AsRef<Path>, kind: CoreKind) -> MihomoResult<Self> {
        Self::discover_for_kind_with_binary(project_root, kind, None)
    }

    /// Discovers launch inputs while honoring a user-selected executable.
    ///
    /// A custom path is checked before managed, bundled, workspace, and `PATH`
    /// candidates.
    ///
    /// # Errors
    ///
    /// Returns an error if an explicit executable is absent or invalid, or when
    /// the selected YAML cannot be read.
    pub fn discover_for_kind_with_binary(
        project_root: impl AsRef<Path>,
        kind: CoreKind,
        preferred_binary: Option<&Path>,
    ) -> MihomoResult<Self> {
        Self::discover_for_kind_with_binary_and_config(project_root, kind, preferred_binary, None)
    }

    /// Discovers launch inputs while overriding the startup configuration file.
    ///
    /// This is used for recovery launches that must locate the same executable
    /// and writable home as normal startup without first parsing the rejected
    /// active profile.
    ///
    /// # Errors
    ///
    /// Returns an error if an explicit executable is absent or invalid, or
    /// when the selected YAML cannot be read.
    pub fn discover_for_kind_with_binary_and_config(
        project_root: impl AsRef<Path>,
        kind: CoreKind,
        preferred_binary: Option<&Path>,
        config_override: Option<&Path>,
    ) -> MihomoResult<Self> {
        let project_root = project_root.as_ref();
        let crate::application_paths::CoreRuntimePaths {
            config_file,
            home_dir,
        } = crate::application_paths::CoreRuntimePaths::discover(
            project_root,
            kind,
            config_override,
        );
        let binary = resolve_core_binary(project_root, kind, preferred_binary, &home_dir, None)?;
        if kind == CoreKind::Mihomo {
            install_bundled_mihomo_data(&home_dir)?;
        }
        Self::for_kind(kind, binary, config_file, home_dir)
    }

    /// Resolves ordinary Mihomo identity for Service recovery without reading source YAML.
    /// Preserves the supplied source path and home; the accepted bundle supplies runtime YAML.
    /// Executable selection follows normal startup rules and rejects privileged executables.
    /// Packaged executable installation is protected by the existing home write lease.
    /// No GeoData is installed and no process is started.
    ///
    /// # Errors
    /// Returns executable discovery, ordinary permission or background worker errors.
    pub async fn discover_ordinary_recovery(
        project_root: PathBuf,
        preferred_binary: Option<PathBuf>,
        config_file: PathBuf,
        home_dir: PathBuf,
    ) -> MihomoResult<Self> {
        tokio::task::spawn_blocking(move || {
            let lease = crate::data_coordinator::DataWriteLease::shared([home_dir.clone()]);
            let binary = resolve_core_binary(
                &project_root,
                CoreKind::Mihomo,
                preferred_binary.as_deref(),
                &home_dir,
                Some(&lease),
            )?;
            crate::verify_ordinary_local_executable(&binary)?;
            let endpoint = MihomoEndpoint::local_ipc(&home_dir)?;
            Ok(Self {
                kind: CoreKind::Mihomo,
                binary,
                config_file,
                home_dir,
                endpoint,
                controller_override: None,
            })
        })
        .await
        .map_err(|_| MihomoError::Process(zenclash_i18n::text("core_page.service.failed")))?
    }

    /// Returns the capabilities guaranteed by the selected runtime core.
    #[must_use]
    pub const fn capabilities(&self) -> CoreCapabilities {
        self.kind.capabilities()
    }

    /// Validates the selected startup file with the selected native core.
    /// Cores without a native validation command are left to the controller
    /// readiness check performed immediately after launch.
    ///
    /// # Errors
    ///
    /// Returns an error when the core validation command cannot run, times out,
    /// or rejects the generated startup configuration.
    pub fn validate_config(&self) -> Result<(), CoreConfigValidationError> {
        if !self.capabilities().config_validation {
            return Ok(());
        }
        CoreConfigValidator::new(self.kind, self.binary.clone(), self.home_dir.clone())
            .validate_file(&self.config_file)
    }
}

fn resolve_core_binary(
    project_root: &Path,
    kind: CoreKind,
    preferred_binary: Option<&Path>,
    home_dir: &Path,
    lease: Option<&crate::data_coordinator::DataWriteLease>,
) -> MihomoResult<PathBuf> {
    Ok({
        if let Some(binary) = preferred_binary {
            if !is_core_binary_candidate(binary) {
                return Err(MihomoError::Process(format!(
                    "首选 {} 文件不可执行：{}",
                    kind.display_name(),
                    binary.display()
                )));
            }
            binary.to_path_buf()
        } else if let Some(managed) = managed_core_binary(home_dir, kind) {
            managed
        } else if let Some(bundled) = bundled_core_binary(kind) {
            match lease {
                Some(lease) => install_bundled_core_with_lease(kind, &bundled, home_dir, lease)?,
                None => install_bundled_core(kind, &bundled, home_dir)?,
            }
        } else {
            [crate::WorkspaceResources::new(project_root).core_binary(kind)]
                .into_iter()
                .find(|candidate| is_core_binary_candidate(candidate))
                .or_else(|| find_core_binary(kind))
                .ok_or_else(|| {
                    MihomoError::Process(format!(
                        "找不到 {}；请在设置 → 运行内核中重新检测或选择文件",
                        kind.display_name()
                    ))
                })?
        }
    })
}

fn endpoint_from_config_file(path: &Path) -> MihomoResult<MihomoEndpoint> {
    let contents = read_profile_bytes(path).map_err(|error| {
        MihomoError::Process(format!("无法读取 Mihomo 配置 {}：{error}", path.display()))
    })?;
    let config: FileControllerConfig = serde_yaml::from_slice(&contents).map_err(|error| {
        MihomoError::Process(format!("无法解析 Mihomo 配置 {}：{error}", path.display()))
    })?;
    let controller = if config.external_controller.trim().is_empty() {
        "127.0.0.1:9090".to_owned()
    } else {
        config.external_controller
    };
    Ok(MihomoEndpoint::new(controller, config.secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ordinary_recovery_identity_ignores_source_yaml_and_preserves_home() {
        const CHILD_ROOT: &str = "ZENCLASH_TEST_RECOVERY_IDENTITY";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let home = root.join("home");
            let binary = root.join(if cfg!(windows) {
                "mihomo.exe"
            } else {
                "mihomo"
            });
            let mode = std::env::var("ZENCLASH_TEST_RECOVERY_IDENTITY_MODE").unwrap();
            let managed = home.join("cores").join(binary.file_name().unwrap());
            let expected = if mode == "managed" { &managed } else { &binary };
            let result = CoreLaunchConfig::discover_ordinary_recovery(
                root.clone(),
                (mode != "managed").then(|| binary.clone()),
                root.join("removed.yaml"),
                home.clone(),
            )
            .await;
            if mode == "setid" {
                assert!(matches!(result, Err(MihomoError::InvalidInput(_))));
            } else {
                let launch = result.unwrap();
                assert_eq!(&launch.binary, expected);
                if mode == "managed" {
                    assert_eq!(
                        super::super::resources::service_core_source(&home).unwrap(),
                        std::fs::canonicalize(&managed).unwrap()
                    );
                }
                assert_eq!(launch.home_dir, home);
                assert_eq!(launch.config_file, root.join("removed.yaml"));
                assert!(!launch.config_file.exists());
                let invalid = CoreLaunchConfig::discover_ordinary_recovery(
                    root.clone(),
                    Some(launch.binary),
                    root.join("invalid.yaml"),
                    home.clone(),
                )
                .await
                .unwrap();
                assert_eq!(invalid.config_file, root.join("invalid.yaml"));
            }
            assert_eq!(
                std::fs::read(home.join("GeoIP.dat")).unwrap(),
                b"accepted geoip"
            );
            assert_eq!(
                std::fs::read(root.join("invalid.yaml")).unwrap(),
                b"tun: [broken"
            );
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "zenclash-recovery-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(root.join("home")).unwrap();
        let binary = root.join(if cfg!(windows) {
            "mihomo.exe"
        } else {
            "mihomo"
        });
        std::fs::write(&binary, b"not executed").unwrap();
        std::fs::write(root.join("invalid.yaml"), b"tun: [broken").unwrap();
        std::fs::write(root.join("home/GeoIP.dat"), b"accepted geoip").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::create_dir_all(root.join("home/cores")).unwrap();
        std::fs::copy(
            &binary,
            root.join("home/cores").join(binary.file_name().unwrap()),
        )
        .unwrap();
        let modes = if cfg!(unix) {
            vec!["ordinary", "invalid-override", "managed", "setid"]
        } else {
            vec!["ordinary", "invalid-override", "managed"]
        };
        for mode in modes {
            #[cfg(unix)]
            if mode == "setid" {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o4755)).unwrap();
            }
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.arg("--exact")
                .arg("managed_core_process::launch_config::tests::ordinary_recovery_identity_ignores_source_yaml_and_preserves_home")
                .env(CHILD_ROOT, &root)
                .env("ZENCLASH_TEST_RECOVERY_IDENTITY_MODE", mode)
                .env_remove("ZENCLASH_CORE_BINARY")
                .env_remove("ZENCLASH_MIHOMO_BINARY");
            if mode == "invalid-override" || mode == "managed" {
                command
                    .env("ZENCLASH_CORE_BINARY", root.join("missing-binary"))
                    .env("ZENCLASH_MIHOMO_BINARY", root.join("missing-mihomo"));
            }
            let output = tokio::task::spawn_blocking(move || command.output())
                .await
                .unwrap()
                .unwrap();
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "child selected no test: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_parser_reads_controller_and_secret() {
        let path = std::env::temp_dir().join(format!(
            "zenclash-endpoint-{}-config.yaml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "external-controller: 127.0.0.1:19090\nsecret: integration-secret\n",
        )
        .unwrap();

        let endpoint = endpoint_from_config_file(&path).unwrap();
        let _ = std::fs::remove_file(path);

        assert_eq!(
            (endpoint.controller.as_str(), endpoint.secret.as_str()),
            ("127.0.0.1:19090", "integration-secret")
        );
    }

    #[test]
    fn config_parser_rejects_oversized_input() {
        let path = std::env::temp_dir().join(format!(
            "zenclash-endpoint-{}-oversized.yaml",
            std::process::id()
        ));
        std::fs::write(&path, vec![b'a'; crate::profiles::MAX_PROFILE_BYTES + 1]).unwrap();

        let error = endpoint_from_config_file(&path).unwrap_err();
        let _ = std::fs::remove_file(path);

        assert!(matches!(error, MihomoError::Process(message) if message.contains("超过 16 MiB")));
    }

    #[test]
    fn workspace_fallback_uses_the_packaged_default_source() {
        assert_eq!(
            crate::WorkspaceResources::new("/workspace").default_profile(),
            Path::new("/workspace/platforms/common/default.yaml")
        );
    }

    #[test]
    fn explicit_recovery_config_bypasses_an_invalid_default_profile() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-recovery-discovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let binary = root.join(if cfg!(windows) {
            "mihomo.exe"
        } else {
            "mihomo"
        });
        let invalid = root.join("platforms/common/default.yaml");
        let recovery = root.join("recovery.yaml");
        std::fs::create_dir_all(invalid.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"test binary").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(&invalid, b"this: [is: invalid").unwrap();
        std::fs::write(&recovery, b"mode: direct\nrules:\n  - MATCH,DIRECT\n").unwrap();

        let launch = CoreLaunchConfig::discover_for_kind_with_binary_and_config(
            &root,
            CoreKind::Mihomo,
            Some(&binary),
            Some(&recovery),
        )
        .unwrap();

        assert_eq!(launch.config_file, recovery);
        std::fs::remove_dir_all(root).unwrap();
    }
}
