//! Coherent controller/configuration facts and actual runtime locations for the status menu.

use crate::runtime_read_source::RuntimeReadSource;

use zenclash_core::{
    CoreObservationIdentity, CoreRuntimeDescriptor, CoreSession, ProxyCatalog, ProxyOperations,
    ProxyVisibility, RuntimeConfig,
};

pub(super) struct TrayRuntimeObservation {
    pub(super) identity: CoreObservationIdentity,
    pub(super) config: RuntimeConfig,
    pub(super) catalog: ProxyCatalog,
    pub(super) descriptor: CoreRuntimeDescriptor,
    pub(super) profile_path: Option<std::path::PathBuf>,
}

impl TrayRuntimeObservation {
    #[cfg(test)]
    pub(super) async fn load(session: &CoreSession) -> Result<Self, String> {
        Self::load_with_source(session, &RuntimeReadSource::default()).await
    }

    pub(super) async fn load_with_source(
        session: &CoreSession,
        source: &RuntimeReadSource,
    ) -> Result<Self, String> {
        let observation = source.capture(session).await?;
        let descriptor = session.runtime_descriptor();
        let profile_path = session.committed_profile_snapshot().profile_path;
        let operations = ProxyOperations::from_observation(&observation);
        let (config, catalog) = tokio::try_join!(
            observation.client().runtime_config(),
            operations.catalog(ProxyVisibility::IncludeHidden),
        )
        .map_err(|error| error.to_string())?;
        observation
            .identity()
            .verify_current()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            identity: observation.identity().clone(),
            config,
            catalog,
            descriptor,
            profile_path,
        })
    }
}

#[cfg(test)]
mod tests;
