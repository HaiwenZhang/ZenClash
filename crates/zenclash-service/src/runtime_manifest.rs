//! In-memory resource provenance adapted from clash-verge-service-ipc staging.
//! Candidate directories are isolated; this planner never mutates accepted files.

use std::collections::{BTreeMap, BTreeSet};

use crate::runtime::{RuntimeError, validate_asset_path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceIdentity {
    pub(crate) len: u64,
    pub(crate) sha256: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheDisposition {
    Inherited,
    MissingProvenance,
    ChangedUrl,
    MissingCache,
    InvalidCache,
    ResourceChanged,
    ParseBudget,
    ByteBudget,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RuntimeManifest {
    pub(crate) assets: BTreeMap<String, SourceIdentity>,
    pub(crate) remote_providers: BTreeMap<String, String>,
    pub(crate) cache_dispositions: BTreeMap<String, CacheDisposition>,
}

pub(crate) struct StagePlan {
    pub(crate) copies: Vec<String>,
    pub(crate) skipped: Vec<String>,
    pub(crate) required_deletes: Vec<String>,
    pub(crate) manifest: RuntimeManifest,
}

/// Plans only candidate-owned writes. Identity comes from completed IPC uploads.
pub(crate) fn plan_stage(
    previous: &RuntimeManifest,
    assets: BTreeMap<String, SourceIdentity>,
    remote: BTreeMap<String, String>,
) -> StagePlan {
    let cache_dispositions = previous
        .cache_dispositions
        .iter()
        .filter(|(path, _)| previous.remote_providers.get(*path) == remote.get(*path))
        .map(|(path, disposition)| (path.clone(), *disposition))
        .collect();
    let mut plan = StagePlan {
        copies: Vec::new(),
        skipped: Vec::new(),
        required_deletes: Vec::new(),
        manifest: RuntimeManifest {
            assets,
            remote_providers: remote,
            cache_dispositions,
        },
    };
    for (path, identity) in &plan.manifest.assets {
        if previous.assets.get(path) == Some(identity) {
            plan.skipped.push(path.clone());
        } else {
            plan.copies.push(path.clone());
        }
    }
    for (path, url) in &plan.manifest.remote_providers {
        if previous.remote_providers.get(path) != Some(url) {
            plan.required_deletes.push(path.clone());
        }
    }
    plan
}

/// Rejects collisions before creating a cache or reading any prior file.
pub(crate) fn declared_remote_providers(
    declared: Vec<(String, String)>,
    assets: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, RuntimeError> {
    let mut remote = BTreeMap::new();
    for (destination, url) in declared {
        if !destination.starts_with("cache/providers/")
            || destination["cache/providers/".len()..].len() != 64
            || !destination["cache/providers/".len()..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || assets.contains(&destination)
        {
            return Err(RuntimeError::Asset);
        }
        if remote
            .get(&destination)
            .is_some_and(|previous| previous != &url)
        {
            return Err(RuntimeError::Asset);
        }
        remote.insert(destination, url);
    }
    for path in assets {
        validate_asset_path(path)?;
    }
    Ok(remote)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_content_same_length_is_planned_for_copy() {
        let old = SourceIdentity {
            len: 8,
            sha256: [1; 32],
        };
        let new = SourceIdentity {
            len: 8,
            sha256: [2; 32],
        };
        let previous = RuntimeManifest {
            assets: BTreeMap::from([("assets/ca".into(), old)]),
            ..Default::default()
        };
        let plan = plan_stage(
            &previous,
            BTreeMap::from([("assets/ca".into(), new)]),
            BTreeMap::new(),
        );
        assert_eq!(plan.copies, ["assets/ca"]);
    }

    #[test]
    fn changing_provider_url_invalidates_only_candidate_cache() {
        let path = format!("cache/providers/{}", "a".repeat(64));
        let previous = RuntimeManifest {
            remote_providers: BTreeMap::from([(path.clone(), "https://old.invalid".into())]),
            ..Default::default()
        };
        let plan = plan_stage(
            &previous,
            BTreeMap::new(),
            BTreeMap::from([(path.clone(), "https://new.invalid".into())]),
        );
        assert_eq!(
            plan.required_deletes.as_slice(),
            std::slice::from_ref(&path)
        );
        assert_eq!(previous.remote_providers[&path], "https://old.invalid");
    }
}
