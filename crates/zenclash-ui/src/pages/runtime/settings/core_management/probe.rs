//! Binary metadata observation; preferences writes and validation admission stay with the host.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Weak},
};
use zenclash_core::{CoreBinaryInfo, CoreKind, CoreLaunchConfig};

#[derive(Default)]
pub(in crate::pages::runtime) struct CoreManagementUiState {
    owner: Arc<()>,
    checking: bool,
    source: String,
    info: Option<CoreBinaryInfo>,
    error: Option<String>,
}

pub(super) struct CoreProbeResult {
    source: String,
    result: Result<CoreBinaryInfo, String>,
}

impl CoreManagementUiState {
    pub(super) fn is_checking(&self) -> bool {
        self.checking
    }
    pub(super) fn source(&self) -> &str {
        &self.source
    }
    pub(super) fn info(&self) -> &Option<CoreBinaryInfo> {
        &self.info
    }
    pub(super) fn error(&self) -> &Option<String> {
        &self.error
    }
    pub(super) fn start_read(
        &mut self,
        runtime: &tokio::runtime::Handle,
        preferred: Option<PathBuf>,
    ) -> BinaryProbeRead {
        self.checking = true;
        self.error = None;
        let task = runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                probe_core_binary(CoreKind::Mihomo, preferred.as_deref())
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "core_management.errors.detection_task",
                    &[("error", error.to_string())],
                )
            })
        });

        BinaryProbeRead {
            owner: Arc::downgrade(&self.owner),
            task,
        }
    }
    pub(super) fn complete_read(&mut self, completion: BinaryProbeCompletion) -> bool {
        if !Weak::ptr_eq(&completion.owner, &Arc::downgrade(&self.owner)) {
            return false;
        }
        self.checking = false;
        match completion.result {
            Ok(result) => {
                self.source = result.source;
                match result.result {
                    Ok(info) => {
                        self.info = Some(info);
                        self.error = None;
                    }
                    Err(error) => {
                        self.info = None;
                        self.error = Some(error);
                    }
                }
            }
            Err(error) => {
                self.info = None;
                self.error = Some(error);
            }
        }
        true
    }
    pub(super) fn begin_validation(&mut self) {
        self.checking = true;
    }
    pub(super) fn accept_validated(&mut self, info: CoreBinaryInfo) {
        self.checking = false;
        self.source = zenclash_i18n::text("core_management.source.custom");
        self.info = Some(info);
        self.error = None;
    }
    pub(super) fn validation_failed(&mut self, error: String) {
        self.checking = false;
        self.error = Some(error);
    }
}
pub(super) struct BinaryProbeRead {
    owner: Weak<()>,
    task: tokio::task::JoinHandle<Result<CoreProbeResult, String>>,
}
pub(super) struct BinaryProbeCompletion {
    owner: Weak<()>,
    result: Result<CoreProbeResult, String>,
}
impl BinaryProbeRead {
    pub(super) async fn finish(self) -> BinaryProbeCompletion {
        let result = self
            .task
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "core_management.errors.detection_task",
                    &[("error", error.to_string())],
                )
            })
            .and_then(|result| result);
        BinaryProbeCompletion {
            owner: self.owner,
            result,
        }
    }
}

fn probe_core_binary(kind: CoreKind, preferred: Option<&Path>) -> CoreProbeResult {
    let source = binary_source(preferred);
    let result = project_root()
        .and_then(|root| {
            CoreLaunchConfig::discover_for_kind_with_binary(root, kind, preferred)
                .map_err(|error| error.to_string())
        })
        .and_then(|launch| {
            zenclash_core::validate_core_binary(kind, launch.binary)
                .map_err(|error| error.to_string())
        });
    CoreProbeResult { source, result }
}

fn project_root() -> Result<PathBuf, String> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| zenclash_i18n::text("core_management.errors.workspace"))
}

fn binary_source(preferred: Option<&Path>) -> String {
    if preferred.is_some() {
        zenclash_i18n::text("core_management.source.custom")
    } else {
        zenclash_i18n::text("core_management.source.automatic")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_receipts_keep_model_identity_and_original_completion_order() {
        let mut current = CoreManagementUiState::default();
        let foreign = CoreManagementUiState::default();
        current.checking = true;
        current.source = "current source".into();
        let read = BinaryProbeRead {
            owner: Arc::downgrade(&foreign.owner),
            task: tokio::spawn(async { Err("foreign probe".into()) }),
        };
        assert!(!current.complete_read(read.finish().await));
        assert!(current.is_checking());
        assert_eq!(current.source(), "current source");
        assert!(current.error().is_none());
        let first = BinaryProbeRead {
            owner: Arc::downgrade(&current.owner),
            task: tokio::spawn(async {
                Ok(CoreProbeResult {
                    source: "first source".into(),
                    result: Err("first probe".into()),
                })
            }),
        };
        let second = BinaryProbeRead {
            owner: Arc::downgrade(&current.owner),
            task: tokio::spawn(async {
                Ok(CoreProbeResult {
                    source: "second source".into(),
                    result: Err("second probe".into()),
                })
            }),
        };
        assert!(current.complete_read(second.finish().await));
        assert_eq!(current.source(), "second source");
        assert!(current.complete_read(first.finish().await));
        assert_eq!(current.source(), "first source");
        assert_eq!(current.error().as_deref(), Some("first probe"));
        let read = BinaryProbeRead {
            owner: Arc::downgrade(&current.owner),
            task: tokio::spawn(async { Err("worker failed".into()) }),
        };
        assert!(current.complete_read(read.finish().await));
        assert_eq!(current.source(), "first source");
        assert_eq!(current.error().as_deref(), Some("worker failed"));
    }

    #[test]
    fn failed_validation_preserves_the_previous_binary_metadata_and_source() {
        let info = CoreBinaryInfo {
            kind: CoreKind::Mihomo,
            path: PathBuf::from("fixture-mihomo"),
            version: "fixture version".into(),
            architecture: "fixture architecture",
        };
        let mut state = CoreManagementUiState {
            source: "existing source".into(),
            info: Some(info.clone()),
            error: Some("existing error".into()),
            ..CoreManagementUiState::default()
        };
        state.begin_validation();
        assert!(state.is_checking());
        assert_eq!(state.error().as_deref(), Some("existing error"));
        state.validation_failed("validation failed".into());
        assert!(!state.is_checking());
        assert_eq!(state.source(), "existing source");
        assert_eq!(state.info(), &Some(info));
        assert_eq!(state.error().as_deref(), Some("validation failed"));
    }
}
