use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::resources::{
    bundled_core_binary, bundled_profile, default_core_home_dir, find_core_binary,
    install_bundled_core, is_core_binary_candidate,
};
use crate::{
    profiles::read_profile_bytes, CoreCapabilities, CoreKind, MihomoEndpoint, MihomoError,
    MihomoResult,
};

/// Resolved inputs used to launch one managed Mihomo process.
#[derive(Clone, Debug)]
pub struct MihomoLaunchConfig {
    /// Runtime core selected explicitly by the user or caller.
    pub kind: CoreKind,
    /// Core executable selected from an override, bundle, workspace, or `PATH`.
    pub binary: PathBuf,
    /// Clash/Mihomo YAML file passed with `-f`.
    pub config_file: PathBuf,
    /// Writable Mihomo data directory passed with `-d`.
    pub home_dir: PathBuf,
    /// Controller endpoint and secret parsed from the configuration.
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

impl MihomoLaunchConfig {
    /// Builds a launch configuration and reads its controller settings from YAML.
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
        let endpoint = endpoint_from_config_file(&config_file)?;
        Ok(Self {
            kind,
            binary: binary.into(),
            config_file,
            home_dir: home_dir.into(),
            endpoint,
            controller_override: None,
        })
    }

    /// Overrides the controller used both by Mihomo and by [`crate::MihomoClient`].
    #[must_use]
    pub fn with_controller_override(mut self, controller: impl Into<String>) -> Self {
        let controller = controller.into();
        self.endpoint.controller.clone_from(&controller);
        self.controller_override = Some(controller);
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
    /// The selected core never falls back silently to another implementation.
    /// This keeps behavior deterministic when meow-rs is selected explicitly.
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
    /// Environment overrides remain authoritative. When no override exists, a
    /// custom path is checked before bundled, workspace, and `PATH` candidates.
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
        let project_root = project_root.as_ref();
        let config_file = std::env::var_os("ZENCLASH_CONFIG")
            .map(PathBuf::from)
            .or_else(bundled_profile)
            .unwrap_or_else(|| project_root.join("examples/19facdf022b.yaml"));
        let home_dir = std::env::var_os("ZENCLASH_CORE_HOME")
            .or_else(|| std::env::var_os(kind.home_environment_variable()))
            .map_or_else(|| default_core_home_dir(project_root, kind), PathBuf::from);
        let binary_override = std::env::var_os("ZENCLASH_CORE_BINARY")
            .map(|value| ("ZENCLASH_CORE_BINARY", PathBuf::from(value)))
            .or_else(|| {
                std::env::var_os(kind.binary_environment_variable())
                    .map(|value| (kind.binary_environment_variable(), PathBuf::from(value)))
            });
        let binary = match binary_override {
            Some((_, binary)) if is_core_binary_candidate(&binary) => binary,
            Some((variable, binary)) => {
                return Err(MihomoError::Process(format!(
                    "{} 指向的 {} 文件不可执行：{}",
                    variable,
                    kind.display_name(),
                    binary.display()
                )));
            }
            None => {
                if let Some(binary) = preferred_binary {
                    if !is_core_binary_candidate(binary) {
                        return Err(MihomoError::Process(format!(
                            "首选 {} 文件不可执行：{}",
                            kind.display_name(),
                            binary.display()
                        )));
                    }
                    binary.to_path_buf()
                } else if let Some(bundled) = bundled_core_binary(kind) {
                    install_bundled_core(kind, &bundled, &home_dir)?
                } else {
                    workspace_core_candidates(project_root, kind)
                        .into_iter()
                        .find(|candidate| is_core_binary_candidate(candidate))
                        .or_else(|| find_core_binary(kind))
                        .ok_or_else(|| {
                            MihomoError::Process(format!(
                                "找不到 {}；请设置 {} 或将 {} 放入 PATH",
                                kind.display_name(),
                                kind.binary_environment_variable(),
                                kind.executable_stem()
                            ))
                        })?
                }
            }
        };
        Self::for_kind(kind, binary, config_file, home_dir)
    }

    /// Returns the capabilities guaranteed by the selected runtime core.
    #[must_use]
    pub const fn capabilities(&self) -> CoreCapabilities {
        self.kind.capabilities()
    }
}

fn workspace_core_candidates(project_root: &Path, kind: CoreKind) -> Vec<PathBuf> {
    let filename = if cfg!(windows) {
        format!("{}.exe", kind.executable_stem())
    } else {
        kind.executable_stem().to_owned()
    };
    match kind {
        CoreKind::Mihomo => vec![project_root.join("bin").join(filename)],
        CoreKind::Meow => vec![
            project_root.join("bin").join(&filename),
            project_root
                .join("examples/meow-rs/target/release")
                .join(&filename),
            project_root
                .join("examples/meow-rs/target/debug")
                .join(filename),
        ],
    }
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
    fn explicit_meow_launch_keeps_the_selected_core_kind() {
        let path = std::env::temp_dir().join(format!(
            "zenclash-endpoint-{}-meow.yaml",
            std::process::id()
        ));
        std::fs::write(&path, "external-controller: 127.0.0.1:19090\n").unwrap();

        let launch =
            MihomoLaunchConfig::for_kind(CoreKind::Meow, "meow", &path, "meow-home").unwrap();
        let _ = std::fs::remove_file(path);

        assert_eq!(launch.kind, CoreKind::Meow);
        assert!(!launch.capabilities().full_config_reload);
    }

    #[test]
    fn meow_workspace_lookup_includes_the_downloaded_example_build() {
        let candidates = workspace_core_candidates(Path::new("/workspace"), CoreKind::Meow);
        let filename = if cfg!(windows) { "meow.exe" } else { "meow" };

        assert!(candidates
            .contains(&Path::new("/workspace/examples/meow-rs/target/release").join(filename)));
    }
}
