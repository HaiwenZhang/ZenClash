use std::path::{Path, PathBuf};

use super::resources::install_bundled_mihomo_data;
use crate::application_paths::CoreRuntimePaths;
use crate::{CoreKind, MihomoError, MihomoResult};

/// User-owned startup paths prepared without locating or copying a core executable.
#[derive(Clone, Debug)]
pub struct MihomoRuntimeResources {
    config_file: PathBuf,
    home_dir: PathBuf,
}

impl MihomoRuntimeResources {
    /// Resolves the ordinary Mihomo home without parsing a profile or seeding resources.
    /// Used when startup failed and the GUI needs service health and maintenance.
    #[must_use]
    pub fn recovery_home(project_root: &Path) -> PathBuf {
        CoreRuntimePaths::discover_home(project_root, CoreKind::Mihomo)
    }

    /// Resolves the existing Mihomo configuration/home rules and seeds packaged GeoData.
    ///
    /// Runs filesystem work on a worker. YAML parsing and kernel validation belong
    /// to the subsequent service initialization transaction; controller settings
    /// are never read to construct a privileged HTTP endpoint.
    ///
    /// # Errors
    /// Returns an error if packaged GeoData cannot be safely seeded, or the worker fails.
    pub async fn prepare(
        project_root: PathBuf,
        config_override: Option<PathBuf>,
    ) -> MihomoResult<Self> {
        tokio::task::spawn_blocking(move || {
            let CoreRuntimePaths {
                config_file,
                home_dir,
            } = CoreRuntimePaths::discover(
                &project_root,
                CoreKind::Mihomo,
                config_override.as_deref(),
            );
            install_bundled_mihomo_data(&home_dir)?;
            Ok(Self {
                config_file,
                home_dir,
            })
        })
        .await
        .map_err(|_| MihomoError::Process(zenclash_i18n::text("core_page.service.failed")))?
    }

    /// Returns the original source profile selected by the normal startup rules.
    #[must_use]
    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    /// Returns the same ordinary-user GeoData and provider home used by local startup.
    #[must_use]
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn startup_resources_prepare_without_local_binary_or_controller_parsing() {
        const CHILD_ROOT: &str = "ZENCLASH_TEST_SERVICE_RESOURCES";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let resources = MihomoRuntimeResources::prepare(root.clone(), None)
                .await
                .unwrap();
            assert_eq!(resources.config_file(), root.join("source.yaml"));
            assert_eq!(resources.home_dir(), root.join("home"));
            assert_eq!(
                std::fs::read(resources.home_dir().join("geoip.metadb")).unwrap(),
                b"existing updated data"
            );
            assert!(!resources.home_dir().join("cores").exists());
            return;
        }
        // Process-scoped environment keeps global test workers and user paths untouched.
        let root = std::env::temp_dir().join(format!(
            "zenclash-service-resources-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::write(
            root.join("source.yaml"),
            "external-controller: [not-an-endpoint]\nmode: rule\n",
        )
        .unwrap();
        std::fs::write(root.join("home/geoip.metadb"), b"existing updated data").unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.arg("--exact").arg("managed_core_process::mihomo_resources::tests::startup_resources_prepare_without_local_binary_or_controller_parsing")
            .env(CHILD_ROOT, &root)
            .env("ZENCLASH_CONFIG", root.join("source.yaml"))
            .env("ZENCLASH_CORE_HOME", root.join("home"))
            .env("ZENCLASH_CORE_BINARY", root.join("missing-mihomo"));
        let output = tokio::task::spawn_blocking(move || command.output())
            .await
            .unwrap()
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
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
}
