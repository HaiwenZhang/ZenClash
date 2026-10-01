//! Shared profile commands for pages, the tray, and automatic updates.

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;
use zenclash_core::{
    ControlledConfigStore, CoreApplyOutcome, CoreCommittedProfileSnapshot, CoreKind,
    CoreRestoreSnapshot, CoreSession, CoreSessionError, EffectiveConfigIntent, MihomoClient,
    ProfileApplication, ProfileApplyOutcome, ProfileChange, ProfileRecovery, ProfileStore,
    RemoteProfileOptions, YamlOverrideStore,
};

/// Shared application-level profile workflow and typed recovery context.
///
/// Clones share the same recovery record. Runtime ordering remains owned by
/// `CoreSession`, and repository transactions remain owned by `ProfileApplication`.
#[derive(Clone)]
pub struct ProfileService {
    session: CoreSession,
    overrides: Option<YamlOverrideStore>,
    failure: Arc<Mutex<ProfileRecoveryState>>,
}

#[derive(Default)]
struct ProfileRecoveryState {
    runtime_result_version: Option<u64>,
    latest_failure: Option<Arc<ProfileApplyOutcome>>,
    unresolved: Option<Arc<ProfileApplyOutcome>>,
}

impl ProfileRecoveryState {
    fn accept_version(&mut self, version: u64) -> bool {
        if self
            .runtime_result_version
            .is_some_and(|published| version <= published)
        {
            return false;
        }
        self.runtime_result_version = Some(version);
        true
    }

    fn clear_failure(&mut self) {
        self.unresolved = None;
        self.latest_failure = None;
    }
}

/// An accepted business result, retaining the complete core receipt.
pub(crate) struct ProfileReceipt {
    path: PathBuf,
    name: String,
    runtime_version: Option<u64>,
    _outcome: Arc<ProfileApplyOutcome>,
}

impl ProfileReceipt {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn runtime_version(&self) -> Option<u64> {
        self.runtime_version
    }
}

#[derive(Debug)]
pub(crate) struct ProfileFailure {
    message: String,
    // Preserve the complete failure including attempted/last-known-good revisions
    // and rollback cause, independently of a page's displayed error string.
    _outcome: Option<Arc<ProfileApplyOutcome>>,
}

impl fmt::Display for ProfileFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<String> for ProfileFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            _outcome: None,
        }
    }
}

#[derive(Clone, Copy)]
enum ChangeContext {
    Selection,
    Update,
    Editor,
}

impl ProfileService {
    /// Composes existing core services without creating another runtime owner.
    #[must_use]
    pub fn new(session: CoreSession, overrides: Option<YamlOverrideStore>) -> Self {
        Self {
            session,
            overrides,
            failure: Arc::new(Mutex::new(ProfileRecoveryState::default())),
        }
    }

    pub(crate) fn session(&self) -> &CoreSession {
        &self.session
    }

    pub(crate) fn client(&self) -> &MihomoClient {
        self.session.client()
    }
    pub(crate) fn kind(&self) -> CoreKind {
        self.session.kind()
    }

    pub(crate) fn latest_recovery(&self) -> Option<ProfileRecovery> {
        match self.failure.lock().unresolved.as_deref() {
            Some(
                ProfileApplyOutcome::RuntimeUnknown { recovery, .. }
                | ProfileApplyOutcome::PersistedButRuntimeUnknown { recovery, .. },
            ) => Some(recovery.clone()),
            _ => None,
        }
    }

    pub(crate) fn committed_profile(&self) -> CoreCommittedProfileSnapshot {
        self.session.committed_profile_snapshot()
    }

    pub(crate) fn is_current(&self, version: u64) -> bool {
        self.session.generation() == version
    }

    pub(crate) async fn enabled_overrides(&self) -> Result<Vec<PathBuf>, String> {
        let store = self.overrides.clone();
        tokio::task::spawn_blocking(move || {
            let store = match store {
                Some(store) => store,
                None => YamlOverrideStore::discover()?,
            };
            store.load_enabled_paths()
        })
        .await
        .map_err(|error| {
            zenclash_i18n::text_with(
                "profiles.errors.override_read_task",
                &[("error", error.to_string())],
            )
        })?
        .map_err(|error| error.to_string())
    }

    pub(crate) async fn import_local(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        source: PathBuf,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        let overrides = self.enabled_overrides().await?;
        self.apply(
            store,
            controlled,
            ProfileChange::ImportLocal { source, overrides },
            ChangeContext::Selection,
        )
        .await
    }

    pub(crate) async fn add_remote(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        name: String,
        url: String,
        user_agent: String,
        options: RemoteProfileOptions,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        let overrides = self.enabled_overrides().await?;
        self.apply(
            store,
            controlled,
            ProfileChange::AddRemote {
                name,
                url,
                user_agent,
                options,
                overrides,
            },
            ChangeContext::Selection,
        )
        .await
    }

    pub(crate) async fn activate(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        id: String,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        let overrides = self.enabled_overrides().await?;
        self.apply(
            store,
            controlled,
            ProfileChange::ActivateExisting { id, overrides },
            ChangeContext::Selection,
        )
        .await
    }

    pub(crate) async fn update_remote(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        id: String,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        let overrides = self.enabled_overrides().await?;
        self.apply(
            store,
            controlled,
            ProfileChange::UpdateRemote { id, overrides },
            ChangeContext::Update,
        )
        .await
    }

    pub(crate) async fn edit_yaml(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        id: String,
        expected_payload: String,
        new_payload: String,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        let overrides = self.enabled_overrides().await?;
        self.apply(
            store,
            controlled,
            ProfileChange::EditYaml {
                id,
                expected_payload,
                new_payload,
                overrides,
            },
            ChangeContext::Editor,
        )
        .await
    }

    async fn apply(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        change: ProfileChange,
        context: ChangeContext,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        let outcome = Arc::new(
            ProfileApplication::new(store, controlled, self.session.clone())
                .apply(change)
                .await,
        );
        self.finish_apply(outcome, context)
    }

    fn finish_apply(
        &self,
        outcome: Arc<ProfileApplyOutcome>,
        context: ChangeContext,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        self.record_outcome(&outcome);
        match outcome.as_ref() {
            ProfileApplyOutcome::Applied {
                profile,
                path,
                runtime_version,
                ..
            } => Ok(ProfileReceipt {
                path: path.clone(),
                name: profile.name.clone(),
                runtime_version: Some(*runtime_version),
                _outcome: outcome,
            }),
            ProfileApplyOutcome::Stored { profile, path, .. } => Ok(ProfileReceipt {
                path: path.clone(),
                name: profile.name.clone(),
                runtime_version: None,
                _outcome: outcome,
            }),
            _ => {
                let message = failure_message(outcome.as_ref(), context, self.kind());
                Err(ProfileFailure {
                    message,
                    _outcome: Some(outcome),
                })
            }
        }
    }

    fn record_outcome(&self, outcome: &Arc<ProfileApplyOutcome>) {
        let version = match outcome.as_ref() {
            ProfileApplyOutcome::Applied {
                runtime_version, ..
            }
            | ProfileApplyOutcome::RolledBack {
                runtime_version, ..
            }
            | ProfileApplyOutcome::RuntimeUnknown {
                runtime_version, ..
            }
            | ProfileApplyOutcome::PersistedButRuntimeUnknown {
                runtime_version, ..
            } => Some(*runtime_version),
            ProfileApplyOutcome::Stored { .. } | ProfileApplyOutcome::Rejected { .. } => None,
        };
        let mut state = self.failure.lock();
        if let Some(version) = version
            && !state.accept_version(version)
        {
            return;
        }
        match outcome.as_ref() {
            ProfileApplyOutcome::Applied { .. } => state.clear_failure(),
            ProfileApplyOutcome::RolledBack { .. } => {
                state.unresolved = None;
                state.latest_failure = Some(Arc::clone(outcome));
            }
            ProfileApplyOutcome::RuntimeUnknown { .. }
            | ProfileApplyOutcome::PersistedButRuntimeUnknown { .. } => {
                state.unresolved = Some(Arc::clone(outcome));
                state.latest_failure = Some(Arc::clone(outcome));
            }
            ProfileApplyOutcome::Rejected { .. } => {
                state.latest_failure = Some(Arc::clone(outcome));
            }
            ProfileApplyOutcome::Stored { .. } => {}
        }
    }

    pub(crate) async fn reload_with_overrides(
        &self,
        controlled: ControlledConfigStore,
        path: &Path,
        overrides: Vec<PathBuf>,
    ) -> Result<CoreApplyOutcome, String> {
        let outcome = self
            .session
            .apply(
                &controlled,
                EffectiveConfigIntent::ActivateProfile {
                    profile: path.to_path_buf(),
                    overrides,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        self.record_accepted_runtime(&outcome);
        Ok(outcome)
    }

    pub(crate) async fn reapply_with_overrides(
        &self,
        controlled: ControlledConfigStore,
        overrides: Vec<PathBuf>,
    ) -> Result<Option<CoreApplyOutcome>, String> {
        match self
            .session
            .apply(
                &controlled,
                EffectiveConfigIntent::ReapplyCurrent { overrides },
            )
            .await
        {
            Ok(outcome) => {
                self.record_accepted_runtime(&outcome);
                Ok(Some(outcome))
            }
            Err(CoreSessionError::NoCommittedProfile) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn record_accepted_runtime(&self, outcome: &CoreApplyOutcome) {
        let mut state = self.failure.lock();
        if state.accept_version(outcome.generation) {
            state.clear_failure();
        }
    }

    pub(crate) async fn restore_snapshot(
        &self,
        controlled: &ControlledConfigStore,
        snapshot: &CoreRestoreSnapshot,
    ) -> Result<CoreApplyOutcome, String> {
        let outcome = self
            .session
            .restore_snapshot(controlled, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        self.record_accepted_runtime(&outcome);
        Ok(outcome)
    }
}

fn failure_message(
    outcome: &ProfileApplyOutcome,
    context: ChangeContext,
    kind: CoreKind,
) -> String {
    let core = kind.display_name().to_owned();
    match outcome {
        ProfileApplyOutcome::Rejected { cause, .. } => cause.to_string(),
        ProfileApplyOutcome::RolledBack { cause, .. } => zenclash_i18n::text_with(
            match context {
                ChangeContext::Selection => "profiles.errors.rejected_rolled_back",
                ChangeContext::Update => "profiles.errors.update_rejected_rolled_back",
                ChangeContext::Editor => "overrides.errors.editor_rejected_rolled_back",
            },
            &[("core", core), ("error", cause.to_string())],
        ),
        ProfileApplyOutcome::RuntimeUnknown { cause, .. } => zenclash_i18n::text_with(
            match context {
                ChangeContext::Editor => "overrides.errors.editor_runtime_unknown",
                _ => "profiles.errors.runtime_unknown",
            },
            &[("core", core), ("error", cause.to_string())],
        ),
        ProfileApplyOutcome::PersistedButRuntimeUnknown {
            cause, rollback, ..
        } => zenclash_i18n::text_with(
            match context {
                ChangeContext::Selection => "profiles.errors.rejected_rollback_failed",
                ChangeContext::Update => "profiles.errors.update_rejected_rollback_failed",
                ChangeContext::Editor => "overrides.errors.editor_rejected_rollback_failed",
            },
            &[
                ("core", core),
                ("error", cause.to_string()),
                ("rollback", rollback.to_string()),
            ],
        ),
        _ => zenclash_i18n::text("profiles.errors.not_applied"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn delayed_acceptance_does_not_clear_a_newer_unknown_runtime() {
        assert_delayed_runtime_publication(false, false).await;
    }

    #[tokio::test]
    async fn delayed_unknown_does_not_replace_a_newer_accepted_runtime() {
        assert_delayed_runtime_publication(true, false).await;
    }

    #[tokio::test]
    async fn reapplying_the_current_profile_clears_shared_runtime_uncertainty() {
        assert_delayed_runtime_publication(true, true).await;
    }

    async fn assert_delayed_runtime_publication(unknown_first: bool, reapply_current: bool) {
        let root = std::env::temp_dir().join(format!(
            "zenclash-profile-result-order-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = ProfileStore::new(root.join("profiles")).unwrap();
        let source = root.join("source.yaml");
        std::fs::write(&source, "mixed-port: 7890\nrules: [MATCH,DIRECT]\n").unwrap();
        let first = store.import_local(&source).unwrap();
        let path = store.activate(&first.id).unwrap();
        std::fs::write(&source, "mixed-port: 7891\nrules: [MATCH,REJECT]\n").unwrap();
        let second = store.import_local(&source).unwrap();
        let overrides = YamlOverrideStore::new(root.join("overrides")).unwrap();
        let controlled = ControlledConfigStore::new(root.join("controlled"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for lose_response in [unknown_first, !unknown_first] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 8192];
                let count = stream.read(&mut request).await.unwrap();
                assert!(request[..count].starts_with(b"PUT /configs"));
                if !lose_response {
                    stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                        .await
                        .unwrap();
                }
                // Losing the response intentionally leaves runtime acceptance uncertain.
            }
        });
        let client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(
            format!("http://{address}"),
            "",
        ))
        .unwrap();
        let session =
            CoreSession::open_with_config(CoreKind::Mihomo, client, None, Some(path), Vec::new());
        let service = ProfileService::new(session.clone(), Some(overrides));
        // Execute a real first transition, then pause its service continuation at the
        // await boundary while another entry point completes and publishes its result.
        let delayed = Arc::new(
            ProfileApplication::new(store.clone(), controlled.clone(), session)
                .apply(ProfileChange::ActivateExisting {
                    id: first.id,
                    overrides: Vec::new(),
                })
                .await,
        );
        assert_eq!(
            matches!(delayed.as_ref(), ProfileApplyOutcome::RuntimeUnknown { .. }),
            unknown_first
        );
        let latest_failed = if reapply_current {
            assert!(
                service
                    .finish_apply(Arc::clone(&delayed), ChangeContext::Selection)
                    .is_err()
            );
            assert!(service.latest_recovery().is_some());
            let applied = service
                .reapply_with_overrides(controlled, Vec::new())
                .await
                .unwrap();
            assert!(applied.is_some());
            assert!(service.latest_recovery().is_none());
            false
        } else {
            service
                .activate(store, controlled, second.id.clone())
                .await
                .is_err()
        };
        server.await.unwrap();
        assert_eq!(latest_failed, !unknown_first);
        let _ = service.finish_apply(delayed, ChangeContext::Selection);
        if unknown_first {
            assert!(
                service.latest_recovery().is_none(),
                "a delayed unknown result replaced the newer accepted runtime"
            );
        } else {
            assert_eq!(
                service
                    .latest_recovery()
                    .map(|recovery| recovery.attempted.profile_id),
                Some(second.id),
                "a delayed accepted result cleared the newer unknown runtime"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn shared_recovery_survives_a_preflight_rejection_until_a_profile_is_accepted() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-profile-service-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = ProfileStore::new(root.join("profiles")).unwrap();
        let source = root.join("source.yaml");
        std::fs::write(&source, "mixed-port: 7890\nrules: [MATCH,DIRECT]\n").unwrap();
        let previous = store.import_local(&source).unwrap();
        let path = store.activate(&previous.id).unwrap();
        std::fs::write(&source, "mixed-port: 7891\nrules: [MATCH,REJECT]\n").unwrap();
        let candidate = store.import_local(&source).unwrap();
        let overrides = YamlOverrideStore::new(root.join("overrides")).unwrap();
        let controlled = ControlledConfigStore::new(root.join("controlled"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            let count = stream.read(&mut request).await.unwrap();
            assert!(request[..count].starts_with(b"PUT /configs?force=true "));
            drop(stream); // The request may have been accepted, but its response is lost.
            let (mut stream, _) = listener.accept().await.unwrap();
            let count = stream.read(&mut request).await.unwrap();
            assert!(request[..count].starts_with(b"PUT /configs?force=true "));
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            let count = stream.read(&mut request).await.unwrap();
            assert!(request[..count].starts_with(b"GET /configs"));
            let body = r#"{"mixed-port":7890,"mode":"rule"}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            let count = stream.read(&mut request).await.unwrap();
            assert!(request[..count].starts_with(b"PATCH /configs"));
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            let count = stream.read(&mut request).await.unwrap();
            assert!(request[..count].starts_with(b"GET /configs"));
            let body = r#"{"mixed-port":7890,"mode":"global"}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        let client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(
            format!("http://{address}"),
            "",
        ))
        .unwrap();
        let session =
            CoreSession::open_with_config(CoreKind::Mihomo, client, None, Some(path), Vec::new());
        let service = ProfileService::new(session.clone(), Some(overrides));
        let other_entry_point = service.clone();
        let failure = service
            .activate(store.clone(), controlled.clone(), candidate.id.clone())
            .await
            .err()
            .unwrap();
        assert!(matches!(
            failure._outcome.as_deref(),
            Some(ProfileApplyOutcome::RuntimeUnknown { .. })
        ));
        let recovery = other_entry_point.latest_recovery().unwrap();
        assert_eq!(recovery.attempted.profile_id, candidate.id);
        assert_eq!(
            recovery.last_known_good.as_ref().unwrap().profile_id,
            previous.id
        );
        assert!(
            other_entry_point
                .activate(store.clone(), controlled.clone(), "missing".into())
                .await
                .is_err()
        );
        assert_eq!(service.latest_recovery(), Some(recovery));
        let accepted = Arc::new(
            ProfileApplication::new(store.clone(), controlled.clone(), session.clone())
                .apply(ProfileChange::ActivateExisting {
                    id: previous.id.clone(),
                    overrides: Vec::new(),
                })
                .await,
        );
        let ProfileApplyOutcome::Applied {
            runtime_version, ..
        } = accepted.as_ref()
        else {
            panic!("the recovery profile was not accepted");
        };
        let applied_version = *runtime_version;
        session.set_mode(&controlled, "global").await.unwrap();
        assert!(session.generation() > applied_version);
        let receipt = other_entry_point
            .finish_apply(accepted, ChangeContext::Selection)
            .unwrap();
        server.await.unwrap();
        assert_eq!(receipt.runtime_version(), Some(applied_version));
        assert!(service.latest_recovery().is_none());
        assert_eq!(store.load().unwrap().active, Some(previous.id));
        std::fs::remove_dir_all(root).unwrap();
    }
}
