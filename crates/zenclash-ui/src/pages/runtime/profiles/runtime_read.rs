//! Controller read policy owned by this capability.
//!
//! The ordinary page reader supplies the original pinned client and verifies it
//! again after this policy returns. The payload grants no publication eligibility.

use crate::pages::runtime::PageSnapshotData;
use zenclash_core::{MihomoClient, ProxyCatalog, RuleCatalog, RuntimeConfig};

fn assemble_profile_snapshot<E: std::fmt::Display>(
    config: Result<RuntimeConfig, E>,
    proxies: Result<ProxyCatalog, E>,
    rules: Result<RuleCatalog, E>,
) -> Result<PageSnapshotData, String> {
    if let (Err(config_error), Err(_), Err(_)) = (&config, &proxies, &rules) {
        return Err(config_error.to_string());
    }
    Ok(PageSnapshotData::Profile {
        config: config.ok(),
        proxy_count: proxies.as_ref().ok().map(|catalog| catalog.proxy_count),
        group_count: proxies.ok().map(|catalog| catalog.groups().len()),
        rule_count: rules.ok().map(|catalog| catalog.rules.len()),
    })
}

pub(in crate::pages::runtime) async fn load(
    client: MihomoClient,
) -> Result<PageSnapshotData, String> {
    let (config, proxies, rules) = tokio::join!(
        client.runtime_config(),
        client.proxy_catalog(),
        client.rule_catalog()
    );
    assemble_profile_snapshot(config, proxies, rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_three_controller_failures_return_error_instead_of_empty_ok() {
        let result = assemble_profile_snapshot(
            Err("controller unreachable"),
            Err("proxies unreachable"),
            Err("rules unreachable"),
        );
        assert_eq!(result.err().as_deref(), Some("controller unreachable"));
    }

    #[test]
    fn partial_controller_success_still_returns_profile_snapshot() {
        let result = assemble_profile_snapshot(
            Ok(RuntimeConfig::default()),
            Err("proxies unreachable"),
            Err("rules unreachable"),
        );
        assert!(matches!(
            result,
            Ok(PageSnapshotData::Profile {
                config: Some(_),
                proxy_count: None,
                group_count: None,
                rule_count: None,
            })
        ));
    }
}
