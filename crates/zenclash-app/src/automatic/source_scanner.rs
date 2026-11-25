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
mod tests;
