//! Source scan facts and metadata-cache policy; no runtime mutation authority.

use std::path::PathBuf;
use zenclash_core::YamlOverrideStore;

#[derive(Clone)]
pub(super) struct PendingSourceRevision {
    pub(super) source_profile: PathBuf,
    pub(super) override_paths: Vec<PathBuf>,
    pub(super) digest: [u8; 32],
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct SourceRevisionKey {
    pub(super) source_profile: PathBuf,
    pub(super) digest: [u8; 32],
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

#[derive(Default)]
pub(super) struct SourceScanner {
    stamps: Vec<SourceFileStamp>,
    cached: Option<Result<Option<PendingSourceRevision>, String>>,
}

impl SourceScanner {
    pub(super) fn scan(
        &mut self,
        store: &zenclash_core::ControlledConfigStore,
        kind: zenclash_core::CoreKind,
        profile: Option<PathBuf>,
    ) -> Result<Option<PendingSourceRevision>, String> {
        let Some(profile) = profile else {
            self.cached = None;
            return Ok(None);
        };
        let overrides = YamlOverrideStore::discover()
            .and_then(|store| store.load_enabled_paths())
            .map_err(|error| error.to_string())?;
        self.scan_with_override_paths(store, kind, profile, overrides)
    }

    fn scan_with_override_paths(
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
    use super::*;
    use zenclash_core::{ControlledConfigStore, CoreKind};

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
}
