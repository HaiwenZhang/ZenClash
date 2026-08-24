use std::{
    collections::VecDeque,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};

use parking_lot::{Mutex, RwLock};
use serde::Deserialize;

use crate::{MihomoClient, MihomoEndpoint, MihomoError, MihomoResult};

const MAX_LOG_LINES: usize = 1_000;

#[derive(Clone, Debug)]
pub struct MihomoLaunchConfig {
    pub binary: PathBuf,
    pub config_file: PathBuf,
    pub home_dir: PathBuf,
    pub endpoint: MihomoEndpoint,
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

    pub fn with_controller_override(mut self, controller: impl Into<String>) -> Self {
        let controller = controller.into();
        self.endpoint.controller = controller.clone();
        self.controller_override = Some(controller);
        self
    }

    pub fn discover(project_root: impl AsRef<Path>) -> MihomoResult<Self> {
        let project_root = project_root.as_ref();
        let config_file = std::env::var_os("ZENCLASH_CONFIG")
            .map(PathBuf::from)
            .or_else(bundled_profile)
            .unwrap_or_else(|| project_root.join("examples/19facdf022b.yaml"));
        let home_dir = std::env::var_os("ZENCLASH_MIHOMO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| default_home_dir(project_root));
        let binary = std::env::var_os("ZENCLASH_MIHOMO_BINARY")
            .map(PathBuf::from)
            .or_else(bundled_mihomo_binary)
            .or_else(|| {
                let candidate = project_root.join("bin/mihomo");
                candidate.is_file().then_some(candidate)
            })
            .or_else(find_mihomo_binary)
            .ok_or_else(|| {
                MihomoError::Process(
                    "找不到 mihomo；请设置 ZENCLASH_MIHOMO_BINARY 或将 mihomo 放入 PATH".into(),
                )
            })?;
        Self::new(binary, config_file, home_dir)
    }
}

pub struct MihomoProcess {
    child: Mutex<Option<Child>>,
    logs: Arc<RwLock<VecDeque<String>>>,
    config: MihomoLaunchConfig,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MihomoProcessSnapshot {
    pub running: bool,
    pub pid: Option<u32>,
    pub binary: PathBuf,
    pub config_file: PathBuf,
    pub home_dir: PathBuf,
    pub logs: Vec<String>,
}

impl MihomoProcess {
    pub fn spawn(config: MihomoLaunchConfig) -> MihomoResult<Arc<Self>> {
        std::fs::create_dir_all(&config.home_dir)
            .map_err(|error| MihomoError::Process(error.to_string()))?;
        let mut command = Command::new(&config.binary);
        command
            .arg("-d")
            .arg(&config.home_dir)
            .arg("-f")
            .arg(&config.config_file);
        if let Some(controller) = &config.controller_override {
            command.arg("-ext-ctl").arg(controller);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                MihomoError::Process(format!("无法启动 {}：{error}", config.binary.display()))
            })?;

        let logs = Arc::new(RwLock::new(VecDeque::new()));
        if let Some(stdout) = child.stdout.take() {
            collect_output(stdout, "INFO", logs.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            collect_output(stderr, "CORE", logs.clone());
        }

        Ok(Arc::new(Self {
            child: Mutex::new(Some(child)),
            logs,
            config,
        }))
    }

    pub async fn wait_until_ready(&self, timeout: Duration) -> MihomoResult<()> {
        let client = MihomoClient::new(self.config.endpoint.clone())?;
        let started = tokio::time::Instant::now();
        loop {
            if client.version().await.is_ok() {
                return Ok(());
            }
            if !self.is_running() {
                let message = self
                    .logs
                    .read()
                    .back()
                    .cloned()
                    .unwrap_or_else(|| "mihomo 在控制器就绪前退出".into());
                return Err(MihomoError::Process(message));
            }
            if started.elapsed() >= timeout {
                return Err(MihomoError::Process(format!(
                    "等待 Mihomo 控制器 {} 超时",
                    self.config.endpoint.controller
                )));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub fn endpoint(&self) -> &MihomoEndpoint {
        &self.config.endpoint
    }

    pub fn is_running(&self) -> bool {
        let mut child = self.child.lock();
        match child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    pub fn snapshot(&self) -> MihomoProcessSnapshot {
        let mut child = self.child.lock();
        let (running, pid) = match child.as_mut() {
            Some(process) => (matches!(process.try_wait(), Ok(None)), Some(process.id())),
            None => (false, None),
        };
        MihomoProcessSnapshot {
            running,
            pid,
            binary: self.config.binary.clone(),
            config_file: self.config.config_file.clone(),
            home_dir: self.config.home_dir.clone(),
            logs: self.logs.read().iter().cloned().collect(),
        }
    }

    pub fn stop(&self) -> MihomoResult<()> {
        let mut child = self.child.lock();
        if let Some(mut process) = child.take() {
            if process
                .try_wait()
                .map_err(|error| MihomoError::Process(error.to_string()))?
                .is_none()
            {
                process
                    .kill()
                    .map_err(|error| MihomoError::Process(error.to_string()))?;
                let _ = process.wait();
            }
        }
        Ok(())
    }
}

impl Drop for MihomoProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn collect_output(
    reader: impl std::io::Read + Send + 'static,
    label: &'static str,
    logs: Arc<RwLock<VecDeque<String>>>,
) {
    std::thread::Builder::new()
        .name(format!("zenclash-mihomo-{label}"))
        .spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let mut logs = logs.write();
                if logs.len() >= MAX_LOG_LINES {
                    logs.pop_front();
                }
                logs.push_back(format!("[{label}] {line}"));
            }
        })
        .expect("failed to start Mihomo output collector");
}

fn endpoint_from_config_file(path: &Path) -> MihomoResult<MihomoEndpoint> {
    let contents = std::fs::read_to_string(path).map_err(|error| {
        MihomoError::Process(format!("无法读取 Mihomo 配置 {}：{error}", path.display()))
    })?;
    let config: FileControllerConfig = serde_yaml::from_str(&contents).map_err(|error| {
        MihomoError::Process(format!("无法解析 Mihomo 配置 {}：{error}", path.display()))
    })?;
    let controller = if config.external_controller.trim().is_empty() {
        "127.0.0.1:9090".to_owned()
    } else {
        config.external_controller
    };
    Ok(MihomoEndpoint::new(controller, config.secret))
}

fn find_mihomo_binary() -> Option<PathBuf> {
    let names = if cfg!(windows) {
        ["mihomo.exe", "mihomo"]
    } else {
        ["mihomo", "mihomo"]
    };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
            .find(|candidate| candidate.is_file())
    })
}

fn bundled_resources_dir() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let macos_dir = executable.parent()?;
    let contents_dir = macos_dir.parent()?;
    let resources = contents_dir.join("Resources");
    resources.is_dir().then_some(resources)
}

fn bundled_mihomo_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    };
    let candidate = bundled_resources_dir()?.join(name);
    candidate.is_file().then_some(candidate)
}

fn bundled_profile() -> Option<PathBuf> {
    let candidate = bundled_resources_dir()?.join("profile.yaml");
    candidate.is_file().then_some(candidate)
}

fn default_home_dir(project_root: &Path) -> PathBuf {
    if bundled_resources_dir().is_some() {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join("Library/Application Support/ZenClash/mihomo");
        }
    }
    project_root.join("target/zenclash-mihomo")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_controller_and_secret_from_yaml() {
        let path = std::env::temp_dir().join(format!(
            "zenclash-endpoint-{}-{}.yaml",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(
            &path,
            "external-controller: 127.0.0.1:19090\nsecret: integration-secret\n",
        )
        .unwrap();
        let endpoint = endpoint_from_config_file(&path).unwrap();
        let _ = std::fs::remove_file(path);
        assert_eq!(endpoint.controller, "127.0.0.1:19090");
        assert_eq!(endpoint.secret, "integration-secret");
    }
}
