//! Source scan facts and metadata-cache policy; no runtime mutation authority.

use std::path::PathBuf;

use zenclash_core::YamlOverrideStore;

/// Pending source-file change detected by [`SourceScanner`].
#[derive(Clone, Debug)]
pub struct PendingSourceRevision {
    /// Active source profile path inspected by the scanner.
    pub source_profile: PathBuf,
    /// Ordered enabled YAML override paths included in the digest.
    pub override_paths: Vec<PathBuf>,
    /// Content digest distinguishing this revision from the active runtime.
    pub digest: [u8; 32],
}

/// Stable identity used to debounce repeated saves of the same revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRevisionKey {
    /// Active source profile path.
    pub source_profile: PathBuf,
    /// Content digest of the effective source inputs.
    pub digest: [u8; 32],
}

#[derive(PartialEq, Eq)]
struct SourceFileStamp {
    path: PathBuf,
    metadata: Option<FileMetadataStamp>,
}

#[derive(PartialEq, Eq)]
struct FileMetadataStamp {
    size_bytes: u64,
    modified_at: std::time::SystemTime,
}

/// Scans the active profile and injected YAML override store for on-disk edits,
/// caching file metadata stamps to avoid redundant YAML synthesis.
#[derive(Default)]
pub struct SourceScanner {
    override_store: Option<YamlOverrideStore>,
    stamps: Vec<SourceFileStamp>,
    cached: Option<Result<Option<PendingSourceRevision>, String>>,
}

impl SourceScanner {
    /// Creates a scanner bound to the injected YAML override store.
    ///
    /// Never performs runtime store discovery.
    #[must_use]
    pub fn new(override_store: Option<YamlOverrideStore>) -> Self {
        Self {
            override_store,
            stamps: Vec::new(),
            cached: None,
        }
    }

    /// Scans `profile` together with enabled overrides from the injected store.
    ///
    /// # Errors
    ///
    /// Returns an error if the injected override store cannot be read or the
    /// controlled configuration store fails to compute a source revision.
    pub fn scan(
        &mut self,
        store: &zenclash_core::ControlledConfigStore,
        kind: zenclash_core::CoreKind,
        profile: Option<PathBuf>,
    ) -> Result<Option<PendingSourceRevision>, String> {
        let Some(profile) = profile else {
            self.cached = None;
            return Ok(None);
        };
        let overrides = match &self.override_store {
            Some(override_store) => override_store
                .load_enabled_paths()
                .map_err(|error| error.to_string())?,
            None => Vec::new(),
        };
        self.scan_with_override_paths(store, kind, profile, overrides)
    }

    /// Scans `profile` using an explicit list of enabled override paths.
    ///
    /// # Errors
    ///
    /// Returns an error if the controlled configuration store fails to compute
    /// the pending source revision.
    pub fn scan_with_override_paths(
        &mut self,
        store: &zenclash_core::ControlledConfigStore,
        kind: zenclash_core::CoreKind,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
    ) -> Result<Option<PendingSourceRevision>, String> {
        let paths = [
            profile.clone(),
            store.runtime_path(),
            store.root().join("override.yaml"),
        ]
        .into_iter()
        .chain(overrides.iter().cloned());
        let stamps = paths
            .map(|path| {
                let metadata = std::fs::metadata(&path).ok().and_then(|metadata| {
                    metadata.modified().ok().map(|modified| FileMetadataStamp {
                        size_bytes: metadata.len(),
                        modified_at: modified,
                    })
                });
                SourceFileStamp { path, metadata }
            })
            .collect::<Vec<_>>();
        if self.stamps == stamps
            && let Some(result) = &self.cached
        {
            return result.clone();
        }
        let result = store
            .pending_source_revision(kind, &profile, &overrides)
            .map(|revision| {
                revision.map(|revision| PendingSourceRevision {
                    source_profile: profile,
                    override_paths: overrides,
                    digest: revision,
                })
            })
            .map_err(|error| error.to_string());
        self.stamps = stamps;
        self.cached = Some(result.clone());
        result
    }
}

#[cfg(test)]
mod tests {
    use zenclash_core::{ControlledConfigStore, CoreKind};

    use super::*;

    #[test]
    fn scanner_returns_the_profile_override_chain_and_digest_it_checked() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-source-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let profile = root.join("profile.yaml");
        let override_path = root.join("override.yaml");
        std::fs::write(&profile, "mode: rule\nrules: ['MATCH,DIRECT']\n").unwrap();
        std::fs::write(&override_path, "mode: direct\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        store
            .materialize_with_overrides_for_core(
                &profile,
                std::slice::from_ref(&override_path),
                CoreKind::Mihomo,
            )
            .unwrap();
        let mut scanner = SourceScanner::default();
        assert!(
            scanner
                .scan_with_override_paths(
                    &store,
                    CoreKind::Mihomo,
                    profile.clone(),
                    vec![override_path.clone()]
                )
                .unwrap()
                .is_none()
        );
        std::fs::write(&override_path, "mode: global\n# different metadata size\n").unwrap();
        let changed = scanner
            .scan_with_override_paths(
                &store,
                CoreKind::Mihomo,
                profile.clone(),
                vec![override_path.clone()],
            )
            .unwrap()
            .unwrap();
        assert_eq!(changed.source_profile, profile);
        assert_eq!(changed.override_paths, vec![override_path.clone()]);
        let retained = scanner
            .scan_with_override_paths(
                &store,
                CoreKind::Mihomo,
                profile.clone(),
                vec![override_path.clone()],
            )
            .unwrap()
            .unwrap();
        assert_eq!(retained.digest, changed.digest);
        store
            .materialize_with_overrides_for_core(&profile, &[override_path], CoreKind::Mihomo)
            .unwrap();
        assert!(
            scanner
                .scan_with_override_paths(&store, CoreKind::Mihomo, profile, changed.override_paths)
                .unwrap()
                .is_none()
        );
        assert!(
            scanner
                .scan(&store, CoreKind::Mihomo, None)
                .unwrap()
                .is_none()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scanner_uses_injected_override_store_without_discovery() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-source-scan-injected-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let profile = root.join("profile.yaml");
        let override_src = root.join("override.yaml");
        std::fs::write(&profile, "mode: rule\nrules: ['MATCH,DIRECT']\n").unwrap();
        std::fs::write(&override_src, "mode: direct\n").unwrap();
        let overrides = YamlOverrideStore::new(root.join("yaml-overrides")).unwrap();
        let imported = overrides.import_paths([override_src]).unwrap();
        assert_eq!(imported.len(), 1);
        let enabled_paths = overrides.load_enabled_paths().unwrap();

        let controlled = ControlledConfigStore::new(root.join("controlled"));
        controlled
            .materialize_with_overrides_for_core(&profile, &[], CoreKind::Mihomo)
            .unwrap();

        let mut without_store = SourceScanner::new(None);
        assert!(
            without_store
                .scan(&controlled, CoreKind::Mihomo, Some(profile.clone()))
                .unwrap()
                .is_none()
        );

        let mut with_store = SourceScanner::new(Some(overrides));
        let pending = with_store
            .scan(&controlled, CoreKind::Mihomo, Some(profile.clone()))
            .unwrap()
            .expect("injected override store must produce a pending revision");
        assert_eq!(pending.source_profile, profile);
        assert_eq!(pending.override_paths, enabled_paths);
        std::fs::remove_dir_all(root).unwrap();
    }
}
