use super::{collect_runtime_bundle, remote_providers_of, resolve_provider_path_conflicts};

#[tokio::test]
async fn conflicting_sources_get_separate_paths_in_config_bundle_and_readback() -> anyhow::Result<()>
{
    let root = std::env::temp_dir().join(format!("zenclash-provider-paths-{}", std::process::id()));
    std::fs::create_dir_all(&root)?;
    let mut config: serde_yaml::Mapping = serde_yaml::from_str(
        "rule-providers:\n  AppleDev:\n    type: http\n    url: https://one.example/AppleDev.yaml\n    path: ./providers/rule/AppleDev.yaml\n  mirror:\n    type: http\n    url: https://two.example/AppleDev.yaml\n    path: providers/rule/AppleDev.yaml\n",
    )?;
    let original = config.clone();
    let root = std::fs::canonicalize(root)?;
    let mut reordered = config.clone();
    reordered["rule-providers"] = serde_yaml::from_str(&format!(
        "mirror:\n  type: http\n  url: https://two.example/AppleDev.yaml\n  path: {}\nAppleDev:\n  type: http\n  url: https://one.example/AppleDev.yaml\n  path: providers/rule/AppleDev.yaml\n",
        root.join("providers/rule/AppleDev.yaml").display(),
    ))?;

    resolve_provider_path_conflicts(&mut config, &root)?;
    let config_file = root.join("config.yaml");
    std::fs::write(&config_file, serde_yaml::to_string(&config)?)?;
    let bundle = collect_runtime_bundle(&config_file, &root.join("mihomo")).await?;
    let paths = &config["rule-providers"];
    assert_ne!(paths["AppleDev"]["path"], paths["mirror"]["path"]);
    resolve_provider_path_conflicts(&mut reordered, &root)?;
    assert_eq!(
        config, reordered,
        "path allocation must not depend on declaration order or path spelling"
    );
    let resolved = config.clone();
    resolve_provider_path_conflicts(&mut config, &root)?;
    assert_eq!(
        config, resolved,
        "resolving an already generated config must be idempotent"
    );

    assert_eq!(
        serde_yaml::from_str::<serde_yaml::Mapping>(&bundle.yaml)?,
        config
    );
    let readback = remote_providers_of(&config, &root)?;
    assert_eq!(bundle.remote_providers.len(), 2);
    for declared in readback {
        assert!(bundle.remote_providers.contains(&declared.provider));
        assert_eq!(
            config[declared.section][&declared.name]["path"],
            declared.provider.destination
        );
        assert_eq!(
            config[declared.section][&declared.name]["url"],
            original[declared.section][&declared.name]["url"]
        );
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn allocated_provider_paths_do_not_claim_another_declared_path() -> anyhow::Result<()> {
    let root = std::env::temp_dir();
    let original: serde_yaml::Mapping = serde_yaml::from_str(
        "proxy-providers:\n  a:\n    type: http\n    url: https://one.example/p.yaml\n    path: ./providers/shared.yaml\nrule-providers:\n  b:\n    type: http\n    url: https://two.example/r.yaml\n    path: ./providers/shared.yaml\n",
    )?;
    let mut config = original.clone();
    resolve_provider_path_conflicts(&mut config, &root)?;
    let allocated = config["proxy-providers"]["a"]["path"].clone();
    let mut config = original;
    config["rule-providers"]["reserved"] = serde_yaml::from_str("type: file\npath: placeholder")?;
    config["rule-providers"]["reserved"]["path"] = allocated.clone();
    resolve_provider_path_conflicts(&mut config, &root)?;
    assert_ne!(config["proxy-providers"]["a"]["path"], allocated);
    assert_ne!(
        config["proxy-providers"]["a"]["path"],
        config["rule-providers"]["b"]["path"]
    );
    assert_eq!(config["rule-providers"]["reserved"]["path"], allocated);
    Ok(())
}

#[test]
fn provider_path_resolution_leaves_shared_sources_and_local_files_unchanged() -> anyhow::Result<()>
{
    let mut config: serde_yaml::Mapping = serde_yaml::from_str(
        "rule-providers:\n  a:\n    type: http\n    url: https://one.example/r.yaml\n    path: ./rules/shared.yaml\n  b:\n    type: http\n    url: https://one.example/r.yaml\n    path: ./rules/shared.yaml\n  local:\n    type: file\n    path: ./rules/local.yaml\n  remote:\n    type: http\n    url: https://two.example/r.yaml\n    path: ./rules/local.yaml\n  mirror:\n    type: http\n    url: https://three.example/r.yaml\n    path: ./rules/local.yaml\n",
    )?;
    let original = config.clone();
    resolve_provider_path_conflicts(&mut config, &std::env::temp_dir())?;
    assert_eq!(config, original);
    Ok(())
}

#[tokio::test]
async fn collects_only_local_providers_and_existing_geo_assets() -> anyhow::Result<()> {
    let root = std::env::temp_dir().join(format!("zenclash-runtime-bundle-{}", std::process::id()));
    std::fs::create_dir_all(root.join("providers"))?;
    std::fs::write(root.join("providers/local.yaml"), b"proxies: []\n")?;
    std::fs::write(root.join("Country.mmdb"), b"geo")?;
    let config = root.join("config.yaml");
    let absolute_local = root.join("providers/local.yaml");
    let absolute_remote = root.join("providers/remote.yaml");
    std::fs::write(
        &config,
        format!(
            "proxy-providers:\n  local:\n    type: file\n    path: {}\n  remote:\n    type: http\n    url: https://example.com/p.yaml\n    path: {}\nrule-providers: {{}}\n",
            absolute_local.display(),
            absolute_remote.display()
        ),
    )?;
    let core = root.join("mihomo");
    std::fs::write(&core, b"core")?;

    let bundle = collect_runtime_bundle(&config, &core).await?;

    let bundled_config: serde_yaml::Value = serde_yaml::from_str(&bundle.yaml)?;
    assert_eq!(
        bundled_config["proxy-providers"]["local"]["path"].as_str(),
        Some("providers/local.yaml")
    );
    assert_eq!(
        bundled_config["proxy-providers"]["remote"]["path"].as_str(),
        Some("providers/remote.yaml")
    );
    assert!(!bundle.yaml.contains(&root.to_string_lossy().into_owned()));
    assert!(
        bundle
            .assets
            .iter()
            .any(|asset| asset.destination == "providers/local.yaml")
    );
    assert!(
        bundle
            .assets
            .iter()
            .any(|asset| asset.destination == "Country.mmdb")
    );
    assert!(
        !bundle
            .assets
            .iter()
            .any(|asset| asset.destination.contains("remote"))
    );
    // Remote declarations preserve service-side cache reuse.
    assert_eq!(
        bundle
            .remote_providers
            .iter()
            .map(|provider| (provider.destination.as_str(), provider.url.as_str()))
            .collect::<Vec<_>>(),
        [("providers/remote.yaml", "https://example.com/p.yaml")],
        "the declaration must pair the rewritten destination with the url, not the original path"
    );
    assert!(
        !bundle
            .assets
            .iter()
            .any(|asset| asset.destination.contains("geoip.dat"))
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn two_names_for_one_remote_file_are_folded_rather_than_refused() -> anyhow::Result<()> {
    // Identical repeats are one service-side file and are safe to fold.
    let root = std::env::temp_dir().join(format!("zenclash-bundle-dup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let config = root.join("config.yaml");
    std::fs::write(
        &config,
        "rule-providers:\n  a:\n    type: http\n    url: https://one.example/x.yaml\n    path: ./rules/x.yaml\n  b:\n    type: http\n    url: https://one.example/x.yaml\n    path: ./rules/x.yaml\n",
    )?;
    let core = root.join("mihomo");
    std::fs::write(&core, b"core")?;

    let bundle = collect_runtime_bundle(&config, &core).await?;

    assert_eq!(bundle.remote_providers.len(), 1, "one file, declared twice");
    assert_eq!(bundle.remote_providers[0].destination, "rules/x.yaml");
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn two_sources_for_one_remote_file_are_refused() -> anyhow::Result<()> {
    let root =
        std::env::temp_dir().join(format!("zenclash-bundle-conflict-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let config = root.join("config.yaml");
    std::fs::write(
        &config,
        "rule-providers:\n  a:\n    type: http\n    url: https://one.example/x.yaml\n    path: ./rules/x.yaml\n  b:\n    type: http\n    url: https://two.example/x.yaml\n    path: ./rules/x.yaml\n",
    )?;
    let core = root.join("mihomo");
    std::fs::write(&core, b"core")?;

    let Err(error) = collect_runtime_bundle(&config, &core).await else {
        anyhow::bail!("two sources cannot own one file");
    };

    assert!(
        error.to_string().contains("two different sources"),
        "{error}"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn an_odd_provider_is_left_alone_instead_of_failing_the_whole_bundle() -> anyhow::Result<()> {
    // Odd providers accepted by mihomo must not block the rest of the bundle.
    let root = std::env::temp_dir().join(format!("zenclash-bundle-odd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let config = root.join("config.yaml");
    std::fs::write(
        &config,
        "rule-providers:\n  nourl:\n    type: http\n    path: ./rules/nourl.yaml\n  missing:\n    type: inline\n    url: https://one.example/i.yaml\n    path: ./rules/missing.yaml\n  good:\n    type: http\n    url: https://one.example/g.yaml\n    path: ./rules/g.yaml\n",
    )?;
    let core = root.join("mihomo");
    std::fs::write(&core, b"core")?;

    let bundle = collect_runtime_bundle(&config, &core).await?;

    assert_eq!(
        bundle.remote_providers.len(),
        1,
        "only the provider that could be classified is declared"
    );
    assert_eq!(bundle.remote_providers[0].destination, "rules/g.yaml");
    let reserialized: serde_yaml::Value = serde_yaml::from_str(&bundle.yaml)?;
    assert_eq!(
        reserialized["rule-providers"]["nourl"]["path"].as_str(),
        Some("./rules/nourl.yaml"),
        "an unclassifiable provider keeps the path the core was given"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn rejects_provider_paths_outside_the_config_root() -> anyhow::Result<()> {
    let root = std::env::temp_dir().join(format!(
        "zenclash-runtime-bundle-outside-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root)?;
    let config = root.join("config.yaml");
    let outside = root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("test root has no parent"))?
        .join(format!("outside-{}.yaml", std::process::id()));
    std::fs::write(
        &config,
        format!(
            "proxy-providers:\n  remote:\n    type: http\n    url: https://example.com/p.yaml\n    path: {}\n",
            outside.display()
        ),
    )?;
    let core = root.join("mihomo");
    std::fs::write(&core, b"core")?;

    let Err(error) = collect_runtime_bundle(&config, &core).await else {
        anyhow::bail!("outside provider path must be rejected");
    };

    assert!(error.to_string().contains("outside the config root"));
    std::fs::remove_dir_all(root)?;
    Ok(())
}
