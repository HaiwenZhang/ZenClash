use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::resources::{
    bundled_mihomo_binary, bundled_profile, default_home_dir, find_mihomo_binary,
    is_mihomo_binary_candidate,
};
use crate::{profiles::read_profile_bytes, MihomoEndpoint, MihomoError, MihomoResult};

/// Resolved inputs used to launch one managed Mihomo process.
#[derive(Clone, Debug)]
pub struct MihomoLaunchConfig {
    /// Mihomo executable selected from an override, bundle, workspace, or `PATH`.
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
        let config_file = config_file.into();
        let endpoint = endpoint_from_config_file(&config_file)?;
        Ok(Self {
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
        let project_root = project_root.as_ref();
        let config_file = std::env::var_os("ZENCLASH_CONFIG")
            .map(PathBuf::from)
            .or_else(bundled_profile)
            .unwrap_or_else(|| project_root.join("examples/19facdf022b.yaml"));
        let home_dir = std::env::var_os("ZENCLASH_MIHOMO_HOME")
            .map_or_else(|| default_home_dir(project_root), PathBuf::from);
        let binary = match std::env::var_os("ZENCLASH_MIHOMO_BINARY").map(PathBuf::from) {
            Some(binary) if is_mihomo_binary_candidate(&binary) => binary,
            Some(binary) => {
                return Err(MihomoError::Process(format!(
                    "ZENCLASH_MIHOMO_BINARY 指向的文件不可执行：{}",
                    binary.display()
                )));
            }
            None => bundled_mihomo_binary()
                .or_else(|| {
                    let candidate = project_root.join("bin/mihomo");
                    is_mihomo_binary_candidate(&candidate).then_some(candidate)
                })
                .or_else(find_mihomo_binary)
                .ok_or_else(|| {
                    MihomoError::Process(
                        "找不到 mihomo；请设置 ZENCLASH_MIHOMO_BINARY 或将 mihomo 放入 PATH".into(),
                    )
                })?,
        };
        Self::new(binary, config_file, home_dir)
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
}
