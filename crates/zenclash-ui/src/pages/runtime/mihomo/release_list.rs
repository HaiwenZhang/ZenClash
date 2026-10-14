//! Release-list observation; installation stays with the existing CoreSession transaction.

use std::sync::{Arc, Weak};
use zenclash_core::{CoreKind, MihomoRelease, MihomoReleaseService};

#[derive(Default)]
pub(in crate::pages::runtime) struct CoreReleaseState {
    owner: Arc<()>,
    releases: Vec<MihomoRelease>,
    loading: bool,
    error: Option<String>,
}

impl CoreReleaseState {
    pub(in crate::pages::runtime) fn releases(&self) -> &[MihomoRelease] {
        &self.releases
    }
    pub(in crate::pages::runtime) fn is_loading(&self) -> bool {
        self.loading
    }
    pub(in crate::pages::runtime) fn error(&self) -> &Option<String> {
        &self.error
    }
    pub(super) fn reject_unsupported(&mut self, kind: CoreKind) {
        self.error = Some(zenclash_i18n::text_with(
            "core_page.errors.release_unsupported",
            &[("core", kind.display_name().to_owned())],
        ));
    }
    pub(super) fn start_read(
        &mut self,
        runtime: &tokio::runtime::Handle,
    ) -> Option<ReleaseListRead> {
        if self.loading {
            return None;
        }
        self.loading = true;
        self.error = None;
        let task = runtime.spawn(async {
            MihomoReleaseService::new()
                .map_err(|error| error.to_string())?
                .releases(12)
                .await
                .map_err(|error| error.to_string())
        });

        Some(ReleaseListRead {
            owner: Arc::downgrade(&self.owner),
            task,
        })
    }
    pub(super) fn complete_read(&mut self, completion: ReleaseListCompletion) -> bool {
        if !Weak::ptr_eq(&completion.owner, &Arc::downgrade(&self.owner)) {
            return false;
        }
        self.loading = false;
        match completion.result {
            Ok(releases) => {
                self.releases = releases;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        true
    }
}
pub(super) struct ReleaseListRead {
    owner: Weak<()>,
    task: tokio::task::JoinHandle<Result<Vec<MihomoRelease>, String>>,
}
pub(super) struct ReleaseListCompletion {
    owner: Weak<()>,
    result: Result<Vec<MihomoRelease>, String>,
}
impl ReleaseListRead {
    pub(super) async fn finish(self) -> ReleaseListCompletion {
        let result = self
            .task
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "core_page.errors.release_list_task",
                    &[("error", error.to_string())],
                )
            })
            .and_then(|result| result);
        ReleaseListCompletion {
            owner: self.owner,
            result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn foreign_release_receipts_cannot_finish_the_current_load() {
        let foreign = CoreReleaseState::default();
        let mut current = CoreReleaseState {
            loading: true,
            error: Some("current feedback".into()),
            ..CoreReleaseState::default()
        };
        assert!(
            current
                .start_read(&tokio::runtime::Handle::current())
                .is_none()
        );
        let read = ReleaseListRead {
            owner: Arc::downgrade(&foreign.owner),
            task: tokio::spawn(async { Ok(Vec::new()) }),
        };
        assert!(!current.complete_read(read.finish().await));
        assert!(current.is_loading());
        assert_eq!(current.error().as_deref(), Some("current feedback"));
        let read = ReleaseListRead {
            owner: Arc::downgrade(&current.owner),
            task: tokio::spawn(async { Err("release read failed".into()) }),
        };
        assert!(current.complete_read(read.finish().await));
        assert!(!current.is_loading());
        assert_eq!(current.error().as_deref(), Some("release read failed"));
    }
}
