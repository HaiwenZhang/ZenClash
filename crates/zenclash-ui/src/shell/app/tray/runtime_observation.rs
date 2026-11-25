//! Coherent controller/configuration facts and actual runtime locations for the status menu.

use crate::foundation::primitives::runtime_read_source::RuntimeReadSource;

use zenclash_core::{
    CoreObservationIdentity, CoreRuntimeDescriptor, CoreSession, ProxyCatalog, ProxyOperations,
    ProxyVisibility, RuntimeConfig,
};

pub(in crate::shell) struct TrayRuntimeObservation {
    pub(in crate::shell) identity: CoreObservationIdentity,
    pub(in crate::shell) config: RuntimeConfig,
    pub(in crate::shell) catalog: ProxyCatalog,
    pub(in crate::shell) descriptor: CoreRuntimeDescriptor,
    pub(in crate::shell) profile_path: Option<std::path::PathBuf>,
}

impl TrayRuntimeObservation {
    #[cfg(test)]
    pub(in crate::shell) async fn load(session: &CoreSession) -> Result<Self, String> {
        Self::load_with_source(session, &RuntimeReadSource::default()).await
    }

    pub(in crate::shell) async fn load_with_source(
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
