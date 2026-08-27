//! Persistent, ordered YAML override management.

use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::profiles::atomic_write;

#[cfg(test)]
mod tests;

const MAX_OVERRIDE_BYTES: usize = 4 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_OVERRIDE_COUNT: usize = 1_024;

/// One managed YAML override in effective application order.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct YamlOverrideRecord {
    /// Stable identifier used by native controls.
    pub id: String,
    /// Original filename shown to the user.
    pub name: String,
    /// Filename relative to the managed override directory.
    pub file_name: String,
    /// Whether this entry participates in effective configuration generation.
    pub enabled: bool,
}

/// Persistent ordered override catalog.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct YamlOverrideCatalog {
    /// Managed overrides in application order.
    pub items: Vec<YamlOverrideRecord>,
}

/// Failures produced by the managed YAML override store.
#[derive(Debug, Error)]
pub enum YamlOverrideError {
    /// Filesystem operation failed.
    #[error("YAML 覆写 I/O 错误：{0}")]
    Io(#[from] std::io::Error),
    /// Persistent manifest encoding failed.
    #[error("YAML 覆写清单无效：{0}")]
    Manifest(#[from] serde_json::Error),
    /// An override is invalid or unsafe to import.
    #[error("YAML 覆写无效：{0}")]
    Invalid(String),
    /// The requested record is absent.
    #[error("找不到 YAML 覆写：{0}")]
    NotFound(String),
    /// A file/manifest transaction could not be safely completed.
    #[error("YAML 覆写事务失败：{0}")]
    Transaction(String),
    /// The platform data directory cannot be resolved.
    #[error("无法确定 ZenClash 数据目录")]
    MissingDataDirectory,
}

/// Result type used by [`YamlOverrideStore`].
pub type YamlOverrideResult<T> = Result<T, YamlOverrideError>;

/// Owns copied YAML overrides and an atomic ordered manifest.
#[derive(Clone, Debug)]
pub struct YamlOverrideStore {
    root: PathBuf,
    transaction: Arc<Mutex<()>>,
}

impl YamlOverrideStore {
    /// Opens the platform-default managed override directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the data directory cannot be found or created.
    pub fn discover() -> YamlOverrideResult<Self> {
        Self::new(default_data_dir()?.join("yaml-overrides"))
    }

    /// Opens a store rooted at `root`.
    ///
    /// # Errors
    ///
    /// Returns an error when its managed files directory cannot be created.
    pub fn new(root: impl Into<PathBuf>) -> YamlOverrideResult<Self> {
        let store = Self {
            root: root.into(),
            transaction: Arc::new(Mutex::new(())),
        };
        fs::create_dir_all(store.files_dir())?;
        Ok(store)
    }

    /// Loads the ordered manifest with a defensive size bound.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O, oversized, or malformed manifests.
    pub fn load(&self) -> YamlOverrideResult<YamlOverrideCatalog> {
        let _transaction = self.transaction.lock();
        self.load_unlocked()
    }

    /// Imports YAML files or the immediate YAML children of directories.
    ///
    /// Every candidate is read and parsed before any managed file is written.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported paths, oversized/non-mapping YAML, or
    /// a failed file/manifest transaction.
    pub fn import_paths(
        &self,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> YamlOverrideResult<Vec<YamlOverrideRecord>> {
        let candidates = collect_candidates(paths)?;
        if candidates.is_empty() {
            return Err(YamlOverrideError::Invalid(
                "没有找到可导入的 .yaml 或 .yml 文件".into(),
            ));
        }
        let _transaction = self.transaction.lock();
        let mut catalog = self.load_unlocked()?;
        let mut imported = Vec::with_capacity(candidates.len());
        let mut created_paths = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if catalog.items.len() >= MAX_OVERRIDE_COUNT {
                cleanup_created_files(&created_paths)?;
                return Err(YamlOverrideError::Invalid(format!(
                    "YAML 覆写数量不能超过 {MAX_OVERRIDE_COUNT}"
                )));
            }
            let id = unique_override_id(&catalog, &candidate.name)?;
            let file_name = format!("{id}.yaml");
            let path = self.files_dir().join(&file_name);
            if let Err(error) = atomic_write(&path, &candidate.payload) {
                cleanup_created_files(&created_paths)?;
                return Err(error.into());
            }
            created_paths.push(path);
            let record = YamlOverrideRecord {
                id,
                name: candidate.name,
                file_name,
                enabled: true,
            };
            catalog.items.push(record.clone());
            imported.push(record);
        }
        if let Err(error) = self.save_unlocked(&catalog) {
            let cleanup_errors = created_paths
                .iter()
                .filter_map(|path| fs::remove_file(path).err())
                .map(|error| error.to_string())
                .collect::<Vec<_>>();
            return if cleanup_errors.is_empty() {
                Err(error)
            } else {
                Err(YamlOverrideError::Transaction(format!(
                    "保存清单失败：{error}；清理已复制文件失败：{}",
                    cleanup_errors.join("；")
                )))
            };
        }
        Ok(imported)
    }

    /// Enables or disables one override while preserving its order.
    ///
    /// # Errors
    ///
    /// Returns an error when the ID is absent or the manifest cannot be saved.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> YamlOverrideResult<()> {
        let _transaction = self.transaction.lock();
        let mut catalog = self.load_unlocked()?;
        let record = catalog
            .items
            .iter_mut()
            .find(|record| record.id == id)
            .ok_or_else(|| YamlOverrideError::NotFound(id.into()))?;
        record.enabled = enabled;
        self.save_unlocked(&catalog)
    }

    /// Moves one entry to an exact zero-based position.
    ///
    /// # Errors
    ///
    /// Returns an error for absent IDs, out-of-range positions, or persistence failure.
    pub fn move_to(&self, id: &str, position: usize) -> YamlOverrideResult<()> {
        let _transaction = self.transaction.lock();
        let mut catalog = self.load_unlocked()?;
        if position >= catalog.items.len() {
            return Err(YamlOverrideError::Invalid(format!(
                "覆写目标顺序 {position} 超出范围"
            )));
        }
        let current = catalog
            .items
            .iter()
            .position(|record| record.id == id)
            .ok_or_else(|| YamlOverrideError::NotFound(id.into()))?;
        let record = catalog.items.remove(current);
        catalog.items.insert(position, record);
        self.save_unlocked(&catalog)
    }

    /// Atomically replaces only ordering/enablement when the catalog is unchanged.
    ///
    /// Both catalogs must contain the exact same managed records; this API is
    /// intended for reversible UI changes around a Mihomo reload.
    ///
    /// # Errors
    ///
    /// Returns an error for concurrent modification, changed record identity,
    /// missing managed files, or persistence failure.
    pub fn replace_catalog(
        &self,
        expected: &YamlOverrideCatalog,
        next: &YamlOverrideCatalog,
    ) -> YamlOverrideResult<()> {
        validate_catalog_reordering(expected, next)?;
        let _transaction = self.transaction.lock();
        let current = self.load_unlocked()?;
        if &current != expected {
            return Err(YamlOverrideError::Transaction(
                "覆写清单已被其他任务修改，请刷新后重试".into(),
            ));
        }
        for record in &next.items {
            if !self.files_dir().join(&record.file_name).is_file() {
                return Err(YamlOverrideError::NotFound(record.file_name.clone()));
            }
        }
        self.save_unlocked(next)
    }

    /// Deletes one managed override with file rollback on manifest failure.
    ///
    /// # Errors
    ///
    /// Returns an error for absent IDs or failed file/manifest transactions.
    pub fn delete(&self, id: &str) -> YamlOverrideResult<()> {
        let _transaction = self.transaction.lock();
        let mut catalog = self.load_unlocked()?;
        let index = catalog
            .items
            .iter()
            .position(|record| record.id == id)
            .ok_or_else(|| YamlOverrideError::NotFound(id.into()))?;
        let record = catalog.items.remove(index);
        let path = self.files_dir().join(&record.file_name);
        let payload = read_bounded(&path, MAX_OVERRIDE_BYTES)?;
        fs::remove_file(&path)?;
        if let Err(error) = self.save_unlocked(&catalog) {
            return match atomic_write(&path, &payload) {
                Ok(()) => Err(error),
                Err(rollback) => Err(YamlOverrideError::Transaction(format!(
                    "保存删除后的清单失败：{error}；恢复覆写文件失败：{rollback}"
                ))),
            };
        }
        Ok(())
    }

    /// Resolves enabled managed files in application order.
    #[must_use]
    pub fn enabled_paths(&self, catalog: &YamlOverrideCatalog) -> Vec<PathBuf> {
        catalog
            .items
            .iter()
            .filter(|record| record.enabled)
            .map(|record| self.files_dir().join(&record.file_name))
            .collect()
    }

    /// Loads and resolves enabled managed files in application order.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest is unreadable or invalid.
    pub fn load_enabled_paths(&self) -> YamlOverrideResult<Vec<PathBuf>> {
        let catalog = self.load()?;
        Ok(self.enabled_paths(&catalog))
    }

    /// Returns the managed override storage root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Quarantines a malformed manifest while retaining every managed YAML file.
    ///
    /// The timestamped manifest can be inspected or restored manually. Only
    /// manifest/validation failures are eligible; filesystem errors remain
    /// visible and are not rewritten.
    ///
    /// # Errors
    ///
    /// Returns the original non-recoverable error or an I/O error while moving
    /// the invalid manifest.
    pub fn quarantine_invalid_manifest(&self) -> YamlOverrideResult<Option<PathBuf>> {
        let _transaction = self.transaction.lock();
        let error = match self.load_unlocked() {
            Ok(_) => return Ok(None),
            Err(error) => error,
        };
        if !matches!(
            error,
            YamlOverrideError::Manifest(_) | YamlOverrideError::Invalid(_)
        ) {
            return Err(error);
        }
        let source = self.manifest_path();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let quarantine = self.root.join(format!(
            "overrides.invalid-{}-{timestamp}.json",
            std::process::id()
        ));
        fs::rename(source, &quarantine)?;
        Ok(Some(quarantine))
    }

    /// Resolves the managed file for a validated catalog record.
    #[must_use]
    pub fn managed_path(&self, record: &YamlOverrideRecord) -> PathBuf {
        self.files_dir().join(&record.file_name)
    }

    fn manifest_path(&self) -> PathBuf {
        self.root.join("overrides.json")
    }

    fn files_dir(&self) -> PathBuf {
        self.root.join("files")
    }

    fn load_unlocked(&self) -> YamlOverrideResult<YamlOverrideCatalog> {
        let path = self.manifest_path();
        if !path.exists() {
            return Ok(YamlOverrideCatalog::default());
        }
        let bytes = read_bounded(&path, MAX_MANIFEST_BYTES)?;
        let catalog = serde_json::from_slice(&bytes)?;
        validate_catalog(&catalog, &self.files_dir())?;
        for record in &catalog.items {
            validate_override_payload(&self.managed_path(record))?;
        }
        Ok(catalog)
    }

    fn save_unlocked(&self, catalog: &YamlOverrideCatalog) -> YamlOverrideResult<()> {
        validate_catalog(catalog, &self.files_dir())?;
        let path = self.manifest_path();
        atomic_write(&path, &serde_json::to_vec_pretty(catalog)?)?;
        Ok(())
    }
}

struct OverrideCandidate {
    name: String,
    payload: Vec<u8>,
}

fn collect_candidates(
    paths: impl IntoIterator<Item = PathBuf>,
) -> YamlOverrideResult<Vec<OverrideCandidate>> {
    let mut files = Vec::new();
    for path in paths {
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(YamlOverrideError::Invalid(format!(
                "{} 是符号链接，不会导入",
                path.display()
            )));
        }
        if path.is_dir() {
            let mut children = Vec::new();
            for entry in fs::read_dir(&path)? {
                let child = entry?.path();
                let metadata = fs::symlink_metadata(&child)?;
                if metadata.file_type().is_symlink() {
                    continue;
                }
                if metadata.is_file() && is_yaml_path(&child) {
                    children.push(child);
                }
            }
            children.sort();
            files.extend(children);
        } else if path.is_file() && is_yaml_path(&path) {
            files.push(path);
        } else {
            return Err(YamlOverrideError::Invalid(format!(
                "{} 不是 YAML 文件或目录",
                path.display()
            )));
        }
    }
    files
        .into_iter()
        .map(|path| {
            let payload = validate_override_payload(&path)?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| YamlOverrideError::Invalid("覆写文件名不是 UTF-8".into()))?
                .to_owned();
            Ok(OverrideCandidate { name, payload })
        })
        .collect()
}

fn validate_override_payload(path: &Path) -> YamlOverrideResult<Vec<u8>> {
    let payload = read_bounded(path, MAX_OVERRIDE_BYTES)?;
    let value: serde_yaml::Value = serde_yaml::from_slice(&payload)
        .map_err(|error| YamlOverrideError::Invalid(format!("{}：{error}", path.display())))?;
    if !value.is_mapping() {
        return Err(YamlOverrideError::Invalid(format!(
            "{} 的 YAML 根节点必须是映射",
            path.display()
        )));
    }
    Ok(payload)
}

fn validate_catalog_reordering(
    expected: &YamlOverrideCatalog,
    next: &YamlOverrideCatalog,
) -> YamlOverrideResult<()> {
    let identity = |catalog: &YamlOverrideCatalog| {
        let mut records = catalog
            .items
            .iter()
            .map(|record| {
                (
                    record.id.clone(),
                    record.name.clone(),
                    record.file_name.clone(),
                )
            })
            .collect::<Vec<_>>();
        records.sort();
        records
    };
    if identity(expected) != identity(next) {
        return Err(YamlOverrideError::Invalid(
            "可逆清单更新不能添加、删除或重命名覆写".into(),
        ));
    }
    Ok(())
}

fn validate_catalog(catalog: &YamlOverrideCatalog, files_dir: &Path) -> YamlOverrideResult<()> {
    if catalog.items.len() > MAX_OVERRIDE_COUNT {
        return Err(YamlOverrideError::Invalid(format!(
            "YAML 覆写数量不能超过 {MAX_OVERRIDE_COUNT}"
        )));
    }
    let mut ids = HashSet::with_capacity(catalog.items.len());
    let mut file_names = HashSet::with_capacity(catalog.items.len());
    for record in &catalog.items {
        if record.id.trim().is_empty() || record.name.trim().is_empty() {
            return Err(YamlOverrideError::Invalid(
                "覆写 ID 和显示名称不能为空".into(),
            ));
        }
        let relative = Path::new(&record.file_name);
        let mut components = relative.components();
        if !matches!(components.next(), Some(Component::Normal(_)))
            || components.next().is_some()
            || !is_yaml_path(relative)
        {
            return Err(YamlOverrideError::Invalid(format!(
                "覆写文件名不安全：{}",
                record.file_name
            )));
        }
        if !ids.insert(record.id.as_str()) || !file_names.insert(record.file_name.as_str()) {
            return Err(YamlOverrideError::Invalid(
                "覆写清单包含重复 ID 或文件名".into(),
            ));
        }
        if !files_dir.join(relative).is_file() {
            return Err(YamlOverrideError::NotFound(record.file_name.clone()));
        }
    }
    Ok(())
}

fn cleanup_created_files(paths: &[PathBuf]) -> YamlOverrideResult<()> {
    let errors = paths
        .iter()
        .filter_map(|path| fs::remove_file(path).err())
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(YamlOverrideError::Transaction(format!(
            "清理未完成的覆写导入失败：{}",
            errors.join("；")
        )))
    }
}

fn read_bounded(path: &Path, limit: usize) -> YamlOverrideResult<Vec<u8>> {
    if fs::metadata(path)?.len() > limit as u64 {
        return Err(oversized(path, limit));
    }
    let bytes = fs::read(path)?;
    if bytes.len() > limit {
        return Err(oversized(path, limit));
    }
    Ok(bytes)
}

fn oversized(path: &Path, limit: usize) -> YamlOverrideError {
    YamlOverrideError::Invalid(format!(
        "{} 超过 {} MiB 限制",
        path.display(),
        limit / 1024 / 1024
    ))
}

fn is_yaml_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "yaml" | "yml"))
}

fn unique_override_id(catalog: &YamlOverrideCatalog, name: &str) -> YamlOverrideResult<String> {
    let stem = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(24)
        .collect::<String>()
        .to_ascii_lowercase();
    let stem = if stem.is_empty() { "override" } else { &stem };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let base = format!("{stem}-{timestamp:x}");
    (0_u32..=u32::MAX)
        .map(|suffix| {
            if suffix == 0 {
                base.clone()
            } else {
                format!("{base}-{suffix}")
            }
        })
        .find(|candidate| catalog.items.iter().all(|record| record.id != *candidate))
        .ok_or_else(|| YamlOverrideError::Transaction("无法生成唯一覆写 ID".into()))
}

fn default_data_dir() -> YamlOverrideResult<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or(YamlOverrideError::MissingDataDirectory)?;
    if cfg!(target_os = "macos") {
        Ok(home.join("Library/Application Support/ZenClash"))
    } else if cfg!(target_os = "windows") {
        Ok(std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or(home.join("AppData/Local"))
            .join("ZenClash"))
    } else {
        Ok(std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or(home.join(".local/share"))
            .join("zenclash"))
    }
}
