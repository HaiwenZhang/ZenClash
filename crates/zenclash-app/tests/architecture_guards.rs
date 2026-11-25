//! CI static architecture boundary guards (Section 6.1 of the v2.0 redesign,
//! Section 5 of the `zenclash-core` 5-layer layout redesign, and Section 5 of
//! the `zenclash-ui` 5-layer layout redesign).
//!
//! Pure-Rust static analysis using only `std` (zero external crates) to enforce:
//! 1. Zero direct bidirectional top-level module cycles across all four business crates
//!    (`zenclash-service`, `zenclash-core`, `zenclash-app`, `zenclash-ui`).
//! 2. Full transitive closure of `zenclash-ui`'s `foundation` and `components` never reaches
//!    `runtime_host`, `features`, or `shell`.
//! 3. Zero runtime `ProfileStore::discover` / `YamlOverrideStore::discover` calls outside
//!    `zenclash-app/src/bootstrap/`.
//! 4. Zero GUI dependencies (`gpui-kit`, `tray-icon`, `objc2`) in `zenclash-app`.
//! 5. Disjoint `HOT_UPDATE_CONFIG_KEYS` and `FULL_RELOAD_CONFIG_KEYS` in `zenclash-core`.
//! 6. Workspace-wide Rust 2024 no-`mod.rs` module layout across all of `crates/`
//!    (`src/` and `tests/`).
//! 7. Strict 5-layer top-level layout in `crates/zenclash-core/src/`.
//! 8. Strict 5-layer downward-only DAG in `zenclash-core` (catching both explicit layer paths
//!    and crate-root re-export shortcuts) plus zero bidirectional submodule cycles inside
//!    `platform`, `mihomo`, `storage`, and `workflows`.
//! 9. Strict 5-layer top-level layout in `crates/zenclash-ui/src/`.
//! 10. Strict 5-layer downward-only DAG in `zenclash-ui` (catching both explicit layer paths
//!     and crate-root re-export shortcuts) plus zero bidirectional submodule cycles inside
//!     `foundation`, `components`, `runtime_host`, and `shell`.
//! 11. Horizontal isolation across sibling features under `crates/zenclash-ui/src/features/`
//!     (except `features/host`).
//! 12. Zero empty directories anywhere under `crates/` (including `crates/zenclash-ui/src/`).
//! 13. Zero `once_cell` dependency in `crates/zenclash-service/Cargo.toml`.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use zenclash_core::{FULL_RELOAD_CONFIG_KEYS, HOT_UPDATE_CONFIG_KEYS};

const ZENCLASH_CORE_LAYERS: [&str; 5] = ["platform", "mihomo", "storage", "runtime", "workflows"];
const ZENCLASH_CORE_STRICTLY_ACYCLIC_LAYERS: [&str; 4] =
    ["platform", "mihomo", "storage", "workflows"];
const ZENCLASH_UI_LAYERS: [&str; 5] = [
    "foundation",
    "components",
    "runtime_host",
    "features",
    "shell",
];
const ZENCLASH_UI_STRICTLY_ACYCLIC_LAYERS: [&str; 4] =
    ["foundation", "components", "runtime_host", "shell"];

#[derive(Clone, Debug, Eq, PartialEq)]
enum Token {
    Ident(String),
    Punct(char),
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root should resolve")
}

#[test]
fn zero_bidirectional_module_cycles_in_core_and_ui() {
    let root = workspace_root();
    let mut all_violations = Vec::new();

    for crate_name in [
        "zenclash-service",
        "zenclash-core",
        "zenclash-app",
        "zenclash-ui",
    ] {
        let src_dir = root.join("crates").join(crate_name).join("src");
        let graph = build_top_level_dependency_graph(&src_dir);
        assert!(
            !graph.modules.is_empty(),
            "expected top-level production modules under {}",
            src_dir.display()
        );

        let mut cycles = Vec::new();
        for (source, targets) in &graph.edges {
            for target in targets {
                if source < target
                    && graph
                        .edges
                        .get(target)
                        .is_some_and(|reverse| reverse.contains(source))
                {
                    let forward_files = graph
                        .edge_files
                        .get(&(source.clone(), target.clone()))
                        .map(|files| files.iter().cloned().collect::<Vec<_>>().join(", "))
                        .unwrap_or_default();
                    let reverse_files = graph
                        .edge_files
                        .get(&(target.clone(), source.clone()))
                        .map(|files| files.iter().cloned().collect::<Vec<_>>().join(", "))
                        .unwrap_or_default();
                    cycles.push(format!(
                        "{crate_name}: {source} <-> {target}\n  {source} -> {target}: [{forward_files}]\n  {target} -> {source}: [{reverse_files}]"
                    ));
                }
            }
        }
        all_violations.extend(cycles);
    }

    assert!(
        all_violations.is_empty(),
        "direct bidirectional top-level module cycles detected:\n{}",
        all_violations.join("\n")
    );
}

#[test]
fn ui_components_and_actions_never_depend_on_pages_or_app_transitively() {
    let ui_src = workspace_root().join("crates/zenclash-ui/src");
    let graph = build_top_level_dependency_graph(&ui_src);

    assert!(
        graph.modules.contains("components"),
        "expected top-level `components` module under {}",
        ui_src.display()
    );
    assert!(
        graph.modules.contains("foundation"),
        "expected top-level `foundation` module under {}",
        ui_src.display()
    );

    let forbidden = ["runtime_host", "features", "shell"];
    let mut violations = Vec::new();

    for entry in ["foundation", "components"] {
        let closure = transitive_closure(&graph.edges, entry);
        for banned in forbidden {
            if closure.contains(banned) {
                let path = shortest_path(&graph.edges, entry, banned)
                    .map(|chain| chain.join(" -> "))
                    .unwrap_or_else(|| format!("{entry} -> ... -> {banned}"));
                violations.push(format!(
                    "`zenclash-ui::{entry}` transitively depends on `{banned}` via: {path}"
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "forbidden UI layer dependencies detected:\n{}",
        violations.join("\n")
    );
}

#[test]
fn no_runtime_store_discover_calls_outside_bootstrap() {
    let root = workspace_root();
    let allowed_bootstrap_dir = root.join("crates/zenclash-app/src/bootstrap");
    let allowed_bootstrap_file = root.join("crates/zenclash-app/src/bootstrap.rs");
    let mut violations = Vec::new();

    for crate_name in ["zenclash-ui", "zenclash-app"] {
        let src_dir = root.join("crates").join(crate_name).join("src");
        let excluded_paths = collect_cfg_test_module_paths(&src_dir);
        let mut files = Vec::new();
        collect_rs_files(&src_dir, &mut files);
        files.sort();

        for file in files {
            if is_test_only_file(&file, &src_dir, &excluded_paths) {
                continue;
            }
            if file == allowed_bootstrap_file || file.starts_with(&allowed_bootstrap_dir) {
                continue;
            }
            let source = fs::read_to_string(&file)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", file.display()));
            let tokens = strip_cfg_test_tokens(&tokenize_code(&source));
            for forbidden in ["ProfileStore", "YamlOverrideStore"] {
                if contains_qualified_call(&tokens, forbidden, "discover") {
                    let rel = file.strip_prefix(&root).unwrap_or(&file);
                    violations.push(format!("{}: {forbidden}::discover()", rel.display()));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "runtime store `::discover()` calls found outside `zenclash-app/src/bootstrap/`:\n{}",
        violations.join("\n")
    );
}

#[test]
fn zenclash_app_has_zero_gui_dependencies() {
    let app_dir = workspace_root().join("crates/zenclash-app");
    let forbidden_manifest = ["gpui-kit", "gpui_kit", "tray-icon", "tray_icon", "objc2"];
    let forbidden_idents = ["gpui_kit", "tray_icon", "objc2"];
    let mut violations = Vec::new();

    let cargo_toml = app_dir.join("Cargo.toml");
    let manifest = fs::read_to_string(&cargo_toml)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", cargo_toml.display()));
    for (line_number, line) in manifest.lines().enumerate() {
        let active = line.split('#').next().unwrap_or("");
        for banned in forbidden_manifest {
            if active.contains(banned) {
                violations.push(format!(
                    "crates/zenclash-app/Cargo.toml:{}: references `{banned}`",
                    line_number + 1
                ));
            }
        }
    }

    let app_src = app_dir.join("src");
    let mut files = Vec::new();
    collect_rs_files(&app_src, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "expected Rust sources under {}",
        app_src.display()
    );

    for file in files {
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", file.display()));
        let tokens = tokenize_code(&source);
        let rel = file.strip_prefix(&app_dir).unwrap_or(&file);
        for token in &tokens {
            if let Token::Ident(ident) = token
                && forbidden_idents.contains(&ident.as_str())
            {
                violations.push(format!(
                    "crates/zenclash-app/{}: references `{ident}`",
                    rel.display()
                ));
            }
        }
        // Also guard against hyphenated crate names in non-comment Rust code (e.g., attributes).
        let stripped = strip_comments(&source);
        for banned in ["gpui-kit", "tray-icon"] {
            if stripped.contains(banned) {
                violations.push(format!(
                    "crates/zenclash-app/{}: references `{banned}`",
                    rel.display()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "GUI dependency references found in `zenclash-app`:\n{}",
        violations.join("\n")
    );
}

#[test]
fn controlled_config_hot_update_and_full_reload_keys_are_disjoint() {
    assert!(
        !HOT_UPDATE_CONFIG_KEYS.is_empty(),
        "HOT_UPDATE_CONFIG_KEYS must be non-empty"
    );
    assert!(
        !FULL_RELOAD_CONFIG_KEYS.is_empty(),
        "FULL_RELOAD_CONFIG_KEYS must be non-empty"
    );

    let mut hot = BTreeSet::new();
    let mut hot_duplicates = Vec::new();
    for &key in HOT_UPDATE_CONFIG_KEYS {
        if !hot.insert(key) {
            hot_duplicates.push(key);
        }
    }
    assert!(
        hot_duplicates.is_empty(),
        "duplicate keys in HOT_UPDATE_CONFIG_KEYS: {hot_duplicates:?}"
    );

    let mut full = BTreeSet::new();
    let mut full_duplicates = Vec::new();
    for &key in FULL_RELOAD_CONFIG_KEYS {
        if !full.insert(key) {
            full_duplicates.push(key);
        }
    }
    assert!(
        full_duplicates.is_empty(),
        "duplicate keys in FULL_RELOAD_CONFIG_KEYS: {full_duplicates:?}"
    );

    let overlap = hot.intersection(&full).copied().collect::<Vec<_>>();
    assert!(
        overlap.is_empty(),
        "HOT_UPDATE_CONFIG_KEYS and FULL_RELOAD_CONFIG_KEYS must be disjoint, found overlap: {overlap:?}"
    );
}

#[test]
fn workspace_src_uses_rust_2024_no_mod_rs_style() {
    let root = workspace_root();
    let crates_dir = root.join("crates");
    let mut files = Vec::new();
    collect_rs_files(&crates_dir, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "expected Rust source files under {}",
        crates_dir.display()
    );

    let mut violations = Vec::new();
    for file in files {
        if file.file_name() == Some(OsStr::new("mod.rs")) {
            let rel = file.strip_prefix(&root).unwrap_or(&file);
            violations.push(rel.display().to_string());
        }
    }

    assert!(
        violations.is_empty(),
        "legacy `mod.rs` files found under `crates/` (use Rust 2024 `<name>.rs` + `<name>/` style instead):\n{}",
        violations.join("\n")
    );
}

#[test]
fn zenclash_core_top_level_layout_is_strictly_five_layers() {
    let core_src = workspace_root().join("crates/zenclash-core/src");
    let allowed: BTreeSet<&str> = [
        "lib.rs",
        "platform",
        "platform.rs",
        "mihomo",
        "mihomo.rs",
        "storage",
        "storage.rs",
        "runtime",
        "runtime.rs",
        "workflows",
        "workflows.rs",
        "test_support",
        "test_support.rs",
    ]
    .into_iter()
    .collect();

    let mut unexpected = Vec::new();
    let entries = fs::read_dir(&core_src)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", core_src.display()));
    for entry in entries {
        let path = entry.expect("directory entry should be readable").path();
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_else(|| panic!("non-UTF-8 entry in {}", core_src.display()));
        if !allowed.contains(name) {
            unexpected.push(name.to_owned());
        }
    }
    unexpected.sort();

    let mut missing = Vec::new();
    if !core_src.join("lib.rs").is_file() {
        missing.push("lib.rs (file)".to_owned());
    }
    for layer in ZENCLASH_CORE_LAYERS {
        if !core_src.join(format!("{layer}.rs")).is_file() {
            missing.push(format!("{layer}.rs (file)"));
        }
        if !core_src.join(layer).is_dir() {
            missing.push(format!("{layer}/ (directory)"));
        }
    }

    assert!(
        unexpected.is_empty() && missing.is_empty(),
        "crates/zenclash-core/src/ top-level layout mismatch:\n  unexpected: {unexpected:?}\n  missing: {missing:?}"
    );
}

#[test]
fn zenclash_core_enforces_strict_five_layer_dag() {
    let root = workspace_root();
    let core_src = root.join("crates/zenclash-core/src");
    let lib_rs = core_src.join("lib.rs");
    let lib_source = fs::read_to_string(&lib_rs)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", lib_rs.display()));
    let lib_tokens = strip_cfg_test_tokens(&tokenize_code(&lib_source));
    let reexports = parse_crate_root_reexports(&lib_tokens, &ZENCLASH_CORE_LAYERS);

    assert!(
        !reexports.is_empty(),
        "expected crate-root re-exports parsed from {}",
        lib_rs.display()
    );
    assert_eq!(
        reexports.get("service").map(String::as_str),
        Some("runtime"),
        "expected `pub use runtime::service::ipc as service;` to map `service` -> `runtime`"
    );
    assert_eq!(
        reexports.get("ai_network").map(String::as_str),
        Some("workflows"),
        "expected `pub use workflows::diagnostics::ai_network;` to map `ai_network` -> `workflows`"
    );
    assert_eq!(
        reexports.get("ServiceManager").map(String::as_str),
        Some("workflows"),
        "expected `ServiceManager` to map to `workflows`"
    );

    let excluded_paths = collect_cfg_test_module_paths(&core_src);
    let mut files = Vec::new();
    collect_rs_files(&core_src, &mut files);
    files.sort();

    let mut upward_violations = Vec::new();
    let mut checked_files = 0usize;

    for file in files {
        if is_test_only_file(&file, &core_src, &excluded_paths) {
            continue;
        }
        let Ok(rel) = file.strip_prefix(&core_src) else {
            continue;
        };
        let Some(owner_layer) = top_level_owner(rel) else {
            continue;
        };
        if layer_rank(&ZENCLASH_CORE_LAYERS, &owner_layer).is_none() {
            continue;
        }

        checked_files += 1;
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", file.display()));
        let production_tokens = strip_cfg_test_tokens(&tokenize_code(&source));
        let depth_from_crate = module_depth_from_root(rel);
        let rel_display = file.strip_prefix(&root).unwrap_or(&file).display();

        for violation in find_upward_layer_violations(
            &production_tokens,
            &owner_layer,
            depth_from_crate,
            &ZENCLASH_CORE_LAYERS,
            &reexports,
        ) {
            upward_violations.push(format!("{rel_display}: {violation}"));
        }
    }

    assert!(
        checked_files > 0,
        "expected production files across the 5 layers under {}",
        core_src.display()
    );
    assert!(
        upward_violations.is_empty(),
        "upward layer dependencies detected in `zenclash-core` (strict DAG: platform[0] < mihomo[1] < storage[2] < runtime[3] < workflows[4]):\n{}",
        upward_violations.join("\n")
    );

    let mut intra_layer_cycles = Vec::new();
    for layer in ZENCLASH_CORE_STRICTLY_ACYCLIC_LAYERS {
        let graph = build_layer_submodule_dependency_graph(&core_src, layer, &excluded_paths);
        assert!(
            !graph.modules.is_empty(),
            "expected submodules inside `zenclash-core::{layer}`"
        );
        for (source, targets) in &graph.edges {
            for target in targets {
                if source < target
                    && graph
                        .edges
                        .get(target)
                        .is_some_and(|reverse| reverse.contains(source))
                {
                    let forward_files = graph
                        .edge_files
                        .get(&(source.clone(), target.clone()))
                        .map(|files| files.iter().cloned().collect::<Vec<_>>().join(", "))
                        .unwrap_or_default();
                    let reverse_files = graph
                        .edge_files
                        .get(&(target.clone(), source.clone()))
                        .map(|files| files.iter().cloned().collect::<Vec<_>>().join(", "))
                        .unwrap_or_default();
                    intra_layer_cycles.push(format!(
                        "zenclash-core::{layer}: {source} <-> {target}\n  {source} -> {target}: [{forward_files}]\n  {target} -> {source}: [{reverse_files}]"
                    ));
                }
            }
        }
    }

    assert!(
        intra_layer_cycles.is_empty(),
        "direct bidirectional submodule cycles detected inside strictly acyclic `zenclash-core` layers:\n{}",
        intra_layer_cycles.join("\n")
    );
}

#[test]
fn zenclash_ui_top_level_layout_is_strictly_five_layers() {
    let ui_src = workspace_root().join("crates/zenclash-ui/src");
    let allowed: BTreeSet<&str> = [
        "lib.rs",
        "main.rs",
        "foundation",
        "foundation.rs",
        "components",
        "components.rs",
        "runtime_host",
        "runtime_host.rs",
        "features",
        "features.rs",
        "shell",
        "shell.rs",
        "test_support",
        "test_support.rs",
    ]
    .into_iter()
    .collect();

    let mut unexpected = Vec::new();
    let entries = fs::read_dir(&ui_src)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", ui_src.display()));
    for entry in entries {
        let path = entry.expect("directory entry should be readable").path();
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_else(|| panic!("non-UTF-8 entry in {}", ui_src.display()));
        if !allowed.contains(name) {
            unexpected.push(name.to_owned());
        }
    }
    unexpected.sort();

    let mut missing = Vec::new();
    for entry_file in ["lib.rs", "main.rs"] {
        if !ui_src.join(entry_file).is_file() {
            missing.push(format!("{entry_file} (file)"));
        }
    }
    for layer in ZENCLASH_UI_LAYERS {
        if !ui_src.join(format!("{layer}.rs")).is_file() {
            missing.push(format!("{layer}.rs (file)"));
        }
        if !ui_src.join(layer).is_dir() {
            missing.push(format!("{layer}/ (directory)"));
        }
    }

    assert!(
        unexpected.is_empty() && missing.is_empty(),
        "crates/zenclash-ui/src/ top-level layout mismatch:\n  unexpected: {unexpected:?}\n  missing: {missing:?}"
    );
}

#[test]
fn zenclash_ui_enforces_strict_five_layer_dag() {
    let root = workspace_root();
    let ui_src = root.join("crates/zenclash-ui/src");
    let lib_rs = ui_src.join("lib.rs");
    let lib_source = fs::read_to_string(&lib_rs)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", lib_rs.display()));
    let lib_tokens = strip_cfg_test_tokens(&tokenize_code(&lib_source));
    let reexports = parse_crate_root_reexports(&lib_tokens, &ZENCLASH_UI_LAYERS);

    assert!(
        !reexports.is_empty(),
        "expected crate-root re-exports parsed from {}",
        lib_rs.display()
    );
    assert_eq!(
        reexports.get("actions").map(String::as_str),
        Some("foundation"),
        "expected `pub use foundation::{{actions, ...}};` to map `actions` -> `foundation`"
    );
    assert_eq!(
        reexports.get("assets").map(String::as_str),
        Some("foundation"),
        "expected `pub use foundation::{{..., assets, ...}};` to map `assets` -> `foundation`"
    );
    assert_eq!(
        reexports.get("design").map(String::as_str),
        Some("foundation"),
        "expected `pub use foundation::{{..., design}};` to map `design` -> `foundation`"
    );
    assert_eq!(
        reexports.get("app").map(String::as_str),
        Some("shell"),
        "expected `pub use shell::app;` to map `app` -> `shell`"
    );
    assert_eq!(
        reexports.get("tray").map(String::as_str),
        Some("shell"),
        "expected `pub use shell::tray::native as tray;` to map `tray` -> `shell`"
    );

    let excluded_paths = collect_cfg_test_module_paths(&ui_src);
    let mut files = Vec::new();
    collect_rs_files(&ui_src, &mut files);
    files.sort();

    let mut upward_violations = Vec::new();
    let mut checked_files = 0usize;

    for file in files {
        if is_test_only_file(&file, &ui_src, &excluded_paths) {
            continue;
        }
        let Ok(rel) = file.strip_prefix(&ui_src) else {
            continue;
        };
        let Some(owner_layer) = top_level_owner(rel) else {
            continue;
        };
        if layer_rank(&ZENCLASH_UI_LAYERS, &owner_layer).is_none() {
            continue;
        }

        checked_files += 1;
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", file.display()));
        let production_tokens = strip_cfg_test_tokens(&tokenize_code(&source));
        let depth_from_crate = module_depth_from_root(rel);
        let rel_display = file.strip_prefix(&root).unwrap_or(&file).display();

        for violation in find_upward_layer_violations(
            &production_tokens,
            &owner_layer,
            depth_from_crate,
            &ZENCLASH_UI_LAYERS,
            &reexports,
        ) {
            upward_violations.push(format!("{rel_display}: {violation}"));
        }
    }

    assert!(
        checked_files > 0,
        "expected production files across the 5 layers under {}",
        ui_src.display()
    );
    assert!(
        upward_violations.is_empty(),
        "upward layer dependencies detected in `zenclash-ui` (strict DAG: foundation[0] < components[1] < runtime_host[2] < features[3] < shell[4]):\n{}",
        upward_violations.join("\n")
    );

    let mut intra_layer_cycles = Vec::new();
    for layer in ZENCLASH_UI_STRICTLY_ACYCLIC_LAYERS {
        let graph = build_layer_submodule_dependency_graph(&ui_src, layer, &excluded_paths);
        assert!(
            !graph.modules.is_empty(),
            "expected submodules inside `zenclash-ui::{layer}`"
        );
        for (source, targets) in &graph.edges {
            for target in targets {
                if source < target
                    && graph
                        .edges
                        .get(target)
                        .is_some_and(|reverse| reverse.contains(source))
                {
                    let forward_files = graph
                        .edge_files
                        .get(&(source.clone(), target.clone()))
                        .map(|files| files.iter().cloned().collect::<Vec<_>>().join(", "))
                        .unwrap_or_default();
                    let reverse_files = graph
                        .edge_files
                        .get(&(target.clone(), source.clone()))
                        .map(|files| files.iter().cloned().collect::<Vec<_>>().join(", "))
                        .unwrap_or_default();
                    intra_layer_cycles.push(format!(
                        "zenclash-ui::{layer}: {source} <-> {target}\n  {source} -> {target}: [{forward_files}]\n  {target} -> {source}: [{reverse_files}]"
                    ));
                }
            }
        }
    }

    assert!(
        intra_layer_cycles.is_empty(),
        "direct bidirectional submodule cycles detected inside strictly acyclic `zenclash-ui` layers:\n{}",
        intra_layer_cycles.join("\n")
    );
}

#[test]
fn zenclash_ui_features_are_horizontally_isolated() {
    let root = workspace_root();
    let ui_src = root.join("crates/zenclash-ui/src");
    let features_dir = ui_src.join("features");
    let excluded_paths = collect_cfg_test_module_paths(&ui_src);

    let mut sibling_features = discover_top_level_modules(&features_dir, &excluded_paths);
    sibling_features.remove("host");
    assert!(
        !sibling_features.is_empty(),
        "expected sibling feature modules under {}",
        features_dir.display()
    );

    let mut files = Vec::new();
    collect_rs_files(&features_dir, &mut files);
    files.sort();

    let mut violations = Vec::new();
    let mut checked_files = 0usize;

    for file in files {
        if is_test_only_file(&file, &ui_src, &excluded_paths) {
            continue;
        }
        let Ok(rel) = file.strip_prefix(&features_dir) else {
            continue;
        };
        let Some(owner_feature) = top_level_owner(rel) else {
            continue;
        };
        if owner_feature == "host" || !sibling_features.contains(&owner_feature) {
            continue;
        }

        checked_files += 1;
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", file.display()));
        let production_tokens = strip_cfg_test_tokens(&tokenize_code(&source));
        let depth_in_features = module_depth_from_root(rel);
        let rel_display = file.strip_prefix(&root).unwrap_or(&file).display();

        for violation in find_horizontal_feature_violations(
            &production_tokens,
            &owner_feature,
            depth_in_features,
            &sibling_features,
        ) {
            violations.push(format!("{rel_display}: {violation}"));
        }
    }

    assert!(
        checked_files > 0,
        "expected production files across sibling features under {}",
        features_dir.display()
    );
    assert!(
        violations.is_empty(),
        "horizontal feature dependencies detected under `crates/zenclash-ui/src/features/` (sibling features must not import one another):\n{}",
        violations.join("\n")
    );
}

#[test]
fn zenclash_ui_has_zero_empty_source_directories() {
    let root = workspace_root();
    let ui_src = root.join("crates/zenclash-ui/src");
    let mut empty_dirs = Vec::new();
    collect_empty_dirs(&ui_src, &mut empty_dirs);
    empty_dirs.sort();

    let formatted = empty_dirs
        .iter()
        .map(|dir| dir.strip_prefix(&root).unwrap_or(dir).display().to_string())
        .collect::<Vec<_>>();

    assert!(
        formatted.is_empty(),
        "empty source directories found under `crates/zenclash-ui/src/`:\n{}",
        formatted.join("\n")
    );
}

#[test]
fn workspace_has_zero_empty_directories() {
    let root = workspace_root();
    let crates_dir = root.join("crates");
    let mut empty_dirs = Vec::new();
    collect_empty_dirs(&crates_dir, &mut empty_dirs);
    empty_dirs.sort();

    let formatted = empty_dirs
        .iter()
        .map(|dir| dir.strip_prefix(&root).unwrap_or(dir).display().to_string())
        .collect::<Vec<_>>();

    assert!(
        formatted.is_empty(),
        "empty directories found under `crates/`:\n{}",
        formatted.join("\n")
    );
}

#[test]
fn zenclash_service_has_no_once_cell_dependency() {
    let cargo_toml = workspace_root().join("crates/zenclash-service/Cargo.toml");
    let manifest = fs::read_to_string(&cargo_toml)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", cargo_toml.display()));
    let mut violations = Vec::new();
    for (line_number, line) in manifest.lines().enumerate() {
        let active = line.split('#').next().unwrap_or("");
        if active.contains("once_cell") || active.contains("once-cell") {
            violations.push(format!(
                "crates/zenclash-service/Cargo.toml:{}: references `once_cell`",
                line_number + 1
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "deprecated `once_cell` dependency found in `zenclash-service` (use `std::sync::OnceLock` instead):\n{}",
        violations.join("\n")
    );
}

struct ModuleGraph {
    modules: BTreeSet<String>,
    edges: BTreeMap<String, BTreeSet<String>>,
    edge_files: BTreeMap<(String, String), BTreeSet<String>>,
}

fn build_top_level_dependency_graph(src_dir: &Path) -> ModuleGraph {
    let excluded_paths = collect_cfg_test_module_paths(src_dir);
    let modules = discover_top_level_modules(src_dir, &excluded_paths);
    let mut edges: BTreeMap<String, BTreeSet<String>> = modules
        .iter()
        .map(|name| (name.clone(), BTreeSet::new()))
        .collect();
    let mut edge_files: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();

    let mut files = Vec::new();
    collect_rs_files(src_dir, &mut files);
    files.sort();

    for file in files {
        if is_test_only_file(&file, src_dir, &excluded_paths) {
            continue;
        }
        let Ok(rel) = file.strip_prefix(src_dir) else {
            continue;
        };
        let Some(owner) = top_level_owner(rel) else {
            continue;
        };
        if !modules.contains(&owner) {
            continue;
        }

        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", file.display()));
        let production_tokens = strip_cfg_test_tokens(&tokenize_code(&source));
        let refs = extract_crate_module_refs(&production_tokens, &modules);
        let rel_display = rel.display().to_string();

        for target in refs {
            if target == owner {
                continue;
            }
            edges
                .entry(owner.clone())
                .or_default()
                .insert(target.clone());
            edge_files
                .entry((owner.clone(), target))
                .or_default()
                .insert(rel_display.clone());
        }
    }

    ModuleGraph {
        modules,
        edges,
        edge_files,
    }
}

fn discover_top_level_modules(
    src_dir: &Path,
    excluded_paths: &BTreeSet<PathBuf>,
) -> BTreeSet<String> {
    let mut modules = BTreeSet::new();
    let entries = fs::read_dir(src_dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", src_dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry should be readable").path();
        if excluded_paths.contains(&path) {
            continue;
        }
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if name == "lib.rs" || name == "main.rs" {
            continue;
        }
        if path.is_dir() {
            if !is_test_component_name(name) {
                modules.insert(name.to_owned());
            }
        } else if path.extension() == Some(OsStr::new("rs"))
            && let Some(stem) = path.file_stem().and_then(OsStr::to_str)
            && !is_test_component_name(stem)
        {
            modules.insert(stem.to_owned());
        }
    }
    modules
}

fn top_level_owner(rel: &Path) -> Option<String> {
    let mut components = rel.components();
    let first = components.next()?.as_os_str().to_str()?;
    let is_single_file = components.next().is_none();
    if is_single_file {
        if first == "lib.rs" || first == "main.rs" {
            return None;
        }
        first.strip_suffix(".rs").map(str::to_owned)
    } else {
        Some(first.to_owned())
    }
}

fn is_test_component_name(name: &str) -> bool {
    name == "tests"
        || name == "ui_tests"
        || name.ends_with("_tests")
        || name.starts_with("test_support")
        || name.ends_with("_test_support")
}

fn is_test_only_file(file: &Path, src_dir: &Path, excluded_paths: &BTreeSet<PathBuf>) -> bool {
    let mut current = Some(file);
    while let Some(path) = current {
        if excluded_paths.contains(path) {
            return true;
        }
        if path == src_dir {
            break;
        }
        current = path.parent();
    }

    let Ok(rel) = file.strip_prefix(src_dir) else {
        return false;
    };
    for component in rel.components() {
        if let Some(name) = component.as_os_str().to_str() {
            let stem = name.strip_suffix(".rs").unwrap_or(name);
            if is_test_component_name(stem) {
                return true;
            }
        }
    }
    false
}

fn collect_rs_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()));
    for entry in entries {
        let path = entry.expect("directory entry should be readable").path();
        if path.is_dir() {
            collect_rs_files(&path, files);
        } else if path.extension() == Some(OsStr::new("rs")) {
            files.push(path);
        }
    }
}

/// Discovers external submodule files/directories declared under `#[cfg(test)]`
/// or `#[cfg(any(test, ...))]` (for example, `#[cfg(test)] mod tests;`).
fn collect_cfg_test_module_paths(src_dir: &Path) -> BTreeSet<PathBuf> {
    let mut excluded = BTreeSet::new();
    let mut files = Vec::new();
    collect_rs_files(src_dir, &mut files);

    for file in files {
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", file.display()));
        let tokens = tokenize_code(&source);
        let child_base = match file.file_name().and_then(OsStr::to_str) {
            Some("mod.rs" | "lib.rs" | "main.rs") => file
                .parent()
                .expect("module file should have a parent directory")
                .to_path_buf(),
            _ => {
                let stem = file
                    .file_stem()
                    .and_then(OsStr::to_str)
                    .expect("Rust file should have a UTF-8 stem");
                file.parent()
                    .expect("Rust file should have a parent directory")
                    .join(stem)
            }
        };

        let mut index = 0;
        while index < tokens.len() {
            if let Some(after_cfg) = match_cfg_test_attr(&tokens, index) {
                let mut cursor = skip_outer_attrs(&tokens, after_cfg);
                cursor = skip_visibility(&tokens, cursor);
                if matches!(tokens.get(cursor), Some(Token::Ident(kw)) if kw == "mod")
                    && let Some(Token::Ident(mod_name)) = tokens.get(cursor + 1)
                    && matches!(tokens.get(cursor + 2), Some(Token::Punct(';')))
                {
                    excluded.insert(child_base.join(format!("{mod_name}.rs")));
                    excluded.insert(child_base.join(mod_name));
                    index = cursor + 3;
                    continue;
                }
            }
            index += 1;
        }
    }
    excluded
}

fn transitive_closure(edges: &BTreeMap<String, BTreeSet<String>>, start: &str) -> BTreeSet<String> {
    let mut visited = BTreeSet::new();
    let mut stack = edges
        .get(start)
        .map(|targets| targets.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();

    while let Some(node) = stack.pop() {
        if visited.contains(&node) {
            continue;
        }
        if let Some(next_targets) = edges.get(&node) {
            for target in next_targets {
                if !visited.contains(target) {
                    stack.push(target.clone());
                }
            }
        }
        visited.insert(node);
    }
    visited
}

fn shortest_path(
    edges: &BTreeMap<String, BTreeSet<String>>,
    start: &str,
    goal: &str,
) -> Option<Vec<String>> {
    use std::collections::VecDeque;

    let mut queue = VecDeque::new();
    let mut visited = BTreeSet::new();
    visited.insert(start.to_owned());
    queue.push_back(vec![start.to_owned()]);

    while let Some(path) = queue.pop_front() {
        let current = path.last()?;
        let Some(neighbors) = edges.get(current) else {
            continue;
        };
        for neighbor in neighbors {
            if neighbor == goal {
                let mut complete = path.clone();
                complete.push(neighbor.clone());
                return Some(complete);
            }
            if visited.insert(neighbor.clone()) {
                let mut next = path.clone();
                next.push(neighbor.clone());
                queue.push_back(next);
            }
        }
    }
    None
}

fn contains_qualified_call(tokens: &[Token], type_name: &str, method_name: &str) -> bool {
    tokens.windows(4).any(|window| {
        matches!(
            window,
            [
                Token::Ident(left),
                Token::Punct(':'),
                Token::Punct(':'),
                Token::Ident(right)
            ] if left == type_name && right == method_name
        )
    })
}

fn extract_crate_module_refs(tokens: &[Token], top_modules: &BTreeSet<String>) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    let mut index = 0;

    while index < tokens.len() {
        // Pattern 1: `use crate::...;`
        if matches!(
            tokens.get(index..index + 4),
            Some([
                Token::Ident(use_kw),
                Token::Ident(crate_kw),
                Token::Punct(':'),
                Token::Punct(':')
            ]) if use_kw == "use" && crate_kw == "crate"
        ) {
            let mut cursor = index + 4;
            match tokens.get(cursor) {
                Some(Token::Punct('{')) => {
                    cursor = parse_use_crate_group(tokens, cursor + 1, top_modules, &mut refs);
                }
                Some(Token::Ident(module_name)) => {
                    if top_modules.contains(module_name) {
                        refs.insert(module_name.clone());
                    }
                    cursor += 1;
                }
                _ => {}
            }
            index = cursor;
            continue;
        }

        // Pattern 2: inline `crate::<top_mod>::...`
        if matches!(
            tokens.get(index..index + 6),
            Some([
                Token::Ident(crate_kw),
                Token::Punct(':'),
                Token::Punct(':'),
                Token::Ident(_),
                Token::Punct(':'),
                Token::Punct(':')
            ]) if crate_kw == "crate"
        ) && let Some(Token::Ident(module_name)) = tokens.get(index + 3)
            && top_modules.contains(module_name)
        {
            refs.insert(module_name.clone());
        }

        index += 1;
    }

    refs
}

fn parse_use_crate_group(
    tokens: &[Token],
    mut cursor: usize,
    top_modules: &BTreeSet<String>,
    refs: &mut BTreeSet<String>,
) -> usize {
    let mut depth = 1usize;
    let mut expect_segment_head = true;

    while cursor < tokens.len() && depth > 0 {
        match &tokens[cursor] {
            Token::Punct('{') => {
                depth += 1;
                expect_segment_head = false;
            }
            Token::Punct('}') => {
                depth -= 1;
            }
            Token::Punct(',') if depth == 1 => {
                expect_segment_head = true;
            }
            Token::Ident(module_name) if depth == 1 && expect_segment_head => {
                if top_modules.contains(module_name) {
                    refs.insert(module_name.clone());
                }
                expect_segment_head = false;
            }
            _ => {}
        }
        cursor += 1;
    }

    cursor
}

fn strip_cfg_test_tokens(tokens: &[Token]) -> Vec<Token> {
    let mut output = Vec::with_capacity(tokens.len());
    let mut index = 0;

    while index < tokens.len() {
        if let Some(after_cfg) = match_cfg_test_attr(tokens, index) {
            index = skip_cfg_test_target(tokens, after_cfg);
            continue;
        }
        output.push(tokens[index].clone());
        index += 1;
    }

    output
}

/// If `tokens[index..]` begins with `#[cfg(test)]` or `#[cfg(any(test, ...))]`,
/// returns the index immediately after the closing `]`.
fn match_cfg_test_attr(tokens: &[Token], index: usize) -> Option<usize> {
    if !matches!(
        tokens.get(index..index + 4),
        Some([
            Token::Punct('#'),
            Token::Punct('['),
            Token::Ident(cfg),
            Token::Punct('(')
        ]) if cfg == "cfg"
    ) {
        return None;
    }

    let mut cursor = index + 4;
    let mut paren_depth = 1usize;
    while cursor < tokens.len() && paren_depth > 0 {
        match tokens[cursor] {
            Token::Punct('(') => paren_depth += 1,
            Token::Punct(')') => paren_depth -= 1,
            _ => {}
        }
        cursor += 1;
    }
    if paren_depth != 0 || !matches!(tokens.get(cursor), Some(Token::Punct(']'))) {
        return None;
    }

    let inner = &tokens[index + 4..cursor - 1];
    let is_test_cfg = match inner {
        [Token::Ident(ident)] if ident == "test" => true,
        [
            Token::Ident(any_kw),
            Token::Punct('('),
            rest @ ..,
            Token::Punct(')'),
        ] if any_kw == "any" => contains_top_level_ident(rest, "test"),
        _ => false,
    };

    is_test_cfg.then_some(cursor + 1)
}

fn contains_top_level_ident(tokens: &[Token], needle: &str) -> bool {
    let mut depth = 0usize;
    for token in tokens {
        match token {
            Token::Punct('(') => depth += 1,
            Token::Punct(')') => depth = depth.saturating_sub(1),
            Token::Ident(ident) if depth == 0 && ident == needle => return true,
            _ => {}
        }
    }
    false
}

fn skip_outer_attrs(tokens: &[Token], mut cursor: usize) -> usize {
    while matches!(
        tokens.get(cursor..cursor + 2),
        Some([Token::Punct('#'), Token::Punct('[')])
    ) {
        cursor += 2;
        let mut bracket_depth = 1usize;
        while cursor < tokens.len() && bracket_depth > 0 {
            match tokens[cursor] {
                Token::Punct('[') => bracket_depth += 1,
                Token::Punct(']') => bracket_depth -= 1,
                _ => {}
            }
            cursor += 1;
        }
    }
    cursor
}

fn skip_visibility(tokens: &[Token], mut cursor: usize) -> usize {
    if matches!(tokens.get(cursor), Some(Token::Ident(kw)) if kw == "pub") {
        cursor += 1;
        if matches!(tokens.get(cursor), Some(Token::Punct('('))) {
            cursor += 1;
            let mut depth = 1usize;
            while cursor < tokens.len() && depth > 0 {
                match tokens[cursor] {
                    Token::Punct('(') => depth += 1,
                    Token::Punct(')') => depth -= 1,
                    _ => {}
                }
                cursor += 1;
            }
        }
    }
    cursor
}

fn is_item_start(tokens: &[Token], start: usize) -> bool {
    let mut cursor = skip_visibility(tokens, start);
    while matches!(
        tokens.get(cursor),
        Some(Token::Ident(kw))
            if matches!(kw.as_str(), "async" | "unsafe" | "const" | "default")
    ) {
        cursor += 1;
    }
    matches!(
        tokens.get(cursor),
        Some(Token::Ident(kw))
            if matches!(
                kw.as_str(),
                "mod"
                    | "fn"
                    | "struct"
                    | "enum"
                    | "union"
                    | "trait"
                    | "impl"
                    | "use"
                    | "type"
                    | "static"
                    | "extern"
            )
    )
}

fn skip_cfg_test_target(tokens: &[Token], after_cfg: usize) -> usize {
    let mut cursor = skip_outer_attrs(tokens, after_cfg);
    if cursor >= tokens.len() {
        return cursor;
    }

    let item = is_item_start(tokens, cursor);
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut seen_brace = false;

    while cursor < tokens.len() {
        match tokens[cursor] {
            Token::Punct('{') => {
                brace_depth += 1;
                seen_brace = true;
            }
            Token::Punct('}') => {
                if brace_depth == 0 {
                    // Hit the closing delimiter of an enclosing struct/block; do not consume it.
                    return cursor;
                }
                brace_depth -= 1;
                if seen_brace && brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                    return cursor + 1;
                }
            }
            Token::Punct('(') => paren_depth += 1,
            Token::Punct(')') => {
                if paren_depth == 0 {
                    return cursor;
                }
                paren_depth -= 1;
            }
            Token::Punct('[') => bracket_depth += 1,
            Token::Punct(']') => {
                if bracket_depth == 0 {
                    return cursor;
                }
                bracket_depth -= 1;
            }
            Token::Punct(';') if !seen_brace && paren_depth == 0 && bracket_depth == 0 => {
                return cursor + 1;
            }
            Token::Punct(',') if !item && !seen_brace && paren_depth == 0 && bracket_depth == 0 => {
                return cursor + 1;
            }
            _ => {}
        }
        cursor += 1;
    }

    cursor
}

/// Strips line and nested block comments while preserving string literals intact.
fn strip_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index = skip_block_comment(bytes, index + 2);
            output.push(' ');
            continue;
        }
        if let Some(next_index) = try_skip_raw_string(bytes, index) {
            output.push_str(&source[index..next_index]);
            index = next_index;
            continue;
        }
        if bytes[index] == b'"' {
            let next_index = skip_quoted_string(bytes, index + 1);
            output.push_str(&source[index..next_index]);
            index = next_index;
            continue;
        }
        let ch = source[index..].chars().next().expect("valid UTF-8 char");
        output.push(ch);
        index += ch.len_utf8();
    }

    output
}

/// Lexes Rust source into identifiers and punctuation while accurately skipping
/// line comments, nested block comments, raw/byte/C/normal strings, and char literals.
fn tokenize_code(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];

        // Whitespace
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }

        // Line comment
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }

        // Nested block comment
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index = skip_block_comment(bytes, index + 2);
            continue;
        }

        // Raw string literal: r#"..."#, br#"..."#, cr#"..."#
        if let Some(next_index) = try_skip_raw_string(bytes, index) {
            index = next_index;
            continue;
        }

        // Byte/C string literal: b"...", c"..."
        if matches!(byte, b'b' | b'c') && bytes.get(index + 1) == Some(&b'"') {
            index = skip_quoted_string(bytes, index + 2);
            continue;
        }

        // Byte char literal: b'...'
        if byte == b'b' && bytes.get(index + 1) == Some(&b'\'') {
            index = skip_char_literal(source, index + 1);
            continue;
        }

        // Normal string literal
        if byte == b'"' {
            index = skip_quoted_string(bytes, index + 1);
            continue;
        }

        // Char literal vs lifetime
        if byte == b'\'' {
            if let Some(next_index) = try_skip_char_literal(source, index) {
                index = next_index;
                continue;
            }
            // Lifetime tick: emit punctuation and continue.
            tokens.push(Token::Punct('\''));
            index += 1;
            continue;
        }

        // Raw identifier: r#ident
        if byte == b'r'
            && bytes.get(index + 1) == Some(&b'#')
            && bytes
                .get(index + 2)
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            let start = index + 2;
            let mut end = start + 1;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            tokens.push(Token::Ident(source[start..end].to_owned()));
            index = end;
            continue;
        }

        // Identifier
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            tokens.push(Token::Ident(source[start..index].to_owned()));
            continue;
        }

        // Punctuation (or non-ASCII char skipped safely)
        if byte.is_ascii_punctuation() {
            tokens.push(Token::Punct(char::from(byte)));
            index += 1;
        } else {
            let ch = source[index..].chars().next().expect("valid UTF-8 char");
            index += ch.len_utf8();
        }
    }

    tokens
}

fn skip_block_comment(bytes: &[u8], mut index: usize) -> usize {
    let mut depth = 1usize;
    while index < bytes.len() && depth > 0 {
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            depth += 1;
            index += 2;
        } else if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
            depth -= 1;
            index += 2;
        } else {
            index += 1;
        }
    }
    index
}

fn try_skip_raw_string(bytes: &[u8], index: usize) -> Option<usize> {
    let r_pos = match bytes[index] {
        b'r' => index,
        b'b' | b'c' if bytes.get(index + 1) == Some(&b'r') => index + 1,
        _ => return None,
    };

    let mut cursor = r_pos + 1;
    while cursor < bytes.len() && bytes[cursor] == b'#' {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }

    let hashes = cursor - (r_pos + 1);
    cursor += 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"' {
            let mut matched = 0;
            while matched < hashes && bytes.get(cursor + 1 + matched) == Some(&b'#') {
                matched += 1;
            }
            if matched == hashes {
                return Some(cursor + 1 + hashes);
            }
        }
        cursor += 1;
    }

    Some(bytes.len())
}

fn skip_quoted_string(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
        } else if bytes[index] == b'"' {
            return index + 1;
        } else {
            index += 1;
        }
    }
    bytes.len()
}

fn try_skip_char_literal(source: &str, index: usize) -> Option<usize> {
    let rest = &source[index + 1..];
    if let Some(after_slash) = rest.strip_prefix('\\') {
        let mut chars = after_slash.char_indices();
        let (_, first) = chars.next()?;
        let mut cursor = index + 2 + first.len_utf8();
        while cursor < source.len() {
            if source.as_bytes()[cursor] == b'\'' {
                return Some(cursor + 1);
            }
            cursor += 1;
        }
        return None;
    }

    let ch = rest.chars().next()?;
    if ch == '\n' || ch == '\'' {
        return None;
    }
    let after_ch = index + 1 + ch.len_utf8();
    if source.as_bytes().get(after_ch) == Some(&b'\'') {
        Some(after_ch + 1)
    } else {
        None
    }
}

fn skip_char_literal(source: &str, quote_index: usize) -> usize {
    try_skip_char_literal(source, quote_index).unwrap_or(quote_index + 1)
}

#[test]
fn lexer_ignores_comments_strings_and_char_literals() {
    let sample = r##"
        // use crate::pages::Page;
        /* crate::app::ZenClashApp /* nested use crate::pages::Page; */ */
        const S: &str = "use crate::pages::Page; ProfileStore::discover()";
        const R: &str = r#"crate::app::Hidden::discover()"#;
        const QUOTE: char = '"';
        use crate::{actions::Quit, design::theme};
        fn f() { let _ = crate::assets::ICON; }
    "##;
    let mods = ["pages", "app", "actions", "design", "assets"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let refs = extract_crate_module_refs(&tokenize_code(sample), &mods);
    assert_eq!(
        refs,
        ["actions", "assets", "design"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    assert!(!contains_qualified_call(
        &tokenize_code(sample),
        "ProfileStore",
        "discover"
    ));
}

#[test]
fn cfg_test_stripping_handles_modules_functions_and_struct_fields() {
    let sample = r#"
        struct Grid {
            #[cfg(test)]
            debug_calls: usize,
            live_field: usize,
        }

        fn prod() {
            let _ = crate::actions::Quit;
        }

        #[cfg(test)]
        mod tests {
            use crate::pages::Page;
            fn helper() {
                let _ = ProfileStore::discover();
            }
        }
    "#;
    let tokens = strip_cfg_test_tokens(&tokenize_code(sample));
    let mods = ["actions", "pages"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        extract_crate_module_refs(&tokens, &mods),
        ["actions"].into_iter().map(str::to_owned).collect()
    );
    assert!(!contains_qualified_call(
        &tokens,
        "ProfileStore",
        "discover"
    ));
    assert!(tokens.contains(&Token::Ident("live_field".into())));
    assert!(!tokens.contains(&Token::Ident("debug_calls".into())));
}

fn layer_rank(layers: &[&str], layer: &str) -> Option<usize> {
    layers.iter().position(|&candidate| candidate == layer)
}

fn module_depth_from_root(rel: &Path) -> usize {
    let count = rel.components().count();
    if rel.file_name() == Some(OsStr::new("mod.rs")) {
        count.saturating_sub(1)
    } else {
        count
    }
}

fn collect_empty_dirs(directory: &Path, empty_dirs: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()));
    let mut entry_count = 0usize;
    for entry in entries {
        entry_count += 1;
        let path = entry.expect("directory entry should be readable").path();
        if path.is_dir() {
            collect_empty_dirs(&path, empty_dirs);
        }
    }
    if entry_count == 0 {
        empty_dirs.push(directory.to_path_buf());
    }
}

/// Parses top-level `pub use <layer>::...;` declarations in a crate's `src/lib.rs`
/// and maps every crate-root re-exported symbol or module alias to its owning layer.
fn parse_crate_root_reexports(tokens: &[Token], layers: &[&str]) -> BTreeMap<String, String> {
    let mut reexports = BTreeMap::new();
    let mut index = 0;
    let mut brace_depth = 0usize;

    while index < tokens.len() {
        match &tokens[index] {
            Token::Punct('{') => {
                brace_depth += 1;
                index += 1;
            }
            Token::Punct('}') => {
                brace_depth = brace_depth.saturating_sub(1);
                index += 1;
            }
            Token::Ident(kw) if brace_depth == 0 && kw == "pub" => {
                let after_vis = skip_visibility(tokens, index);
                if matches!(tokens.get(after_vis), Some(Token::Ident(use_kw)) if use_kw == "use") {
                    let next = parse_use_tree_reexports(
                        tokens,
                        after_vis + 1,
                        &[],
                        layers,
                        &mut reexports,
                    );
                    index = if matches!(tokens.get(next), Some(Token::Punct(';'))) {
                        next + 1
                    } else {
                        next.max(index + 1)
                    };
                } else {
                    index += 1;
                }
            }
            _ => {
                index += 1;
            }
        }
    }

    reexports
}

fn parse_use_tree_reexports(
    tokens: &[Token],
    mut cursor: usize,
    prefix: &[String],
    layers: &[&str],
    out: &mut BTreeMap<String, String>,
) -> usize {
    if matches!(
        tokens.get(cursor..cursor + 2),
        Some([Token::Punct(':'), Token::Punct(':')])
    ) {
        cursor += 2;
    }

    let mut path = prefix.to_vec();
    loop {
        match tokens.get(cursor) {
            Some(Token::Punct('{')) => {
                cursor += 1;
                while cursor < tokens.len() {
                    if matches!(tokens.get(cursor), Some(Token::Punct('}'))) {
                        return cursor + 1;
                    }
                    if matches!(tokens.get(cursor), Some(Token::Punct(','))) {
                        cursor += 1;
                        continue;
                    }
                    let next = parse_use_tree_reexports(tokens, cursor, &path, layers, out);
                    if next <= cursor {
                        cursor += 1;
                    } else {
                        cursor = next;
                    }
                }
                return cursor;
            }
            Some(Token::Punct('*')) => {
                return cursor + 1;
            }
            Some(Token::Ident(segment)) => {
                path.push(segment.clone());
                cursor += 1;
                if matches!(
                    tokens.get(cursor..cursor + 2),
                    Some([Token::Punct(':'), Token::Punct(':')])
                ) {
                    cursor += 2;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    let alias = if matches!(tokens.get(cursor), Some(Token::Ident(as_kw)) if as_kw == "as")
        && let Some(Token::Ident(alias_name)) = tokens.get(cursor + 1)
    {
        cursor += 2;
        Some(alias_name.as_str())
    } else {
        None
    };

    let mut slice: &[String] = &path;
    while slice.len() > 1 && matches!(slice[0].as_str(), "crate" | "self") {
        slice = &slice[1..];
    }
    if slice.len() > 1 && slice.last().is_some_and(|seg| seg == "self") {
        slice = &slice[..slice.len() - 1];
    }

    if let Some(layer) = slice.first()
        && layer_rank(layers, layer).is_some()
        && let Some(exported) = alias.or_else(|| slice.last().map(String::as_str))
        && exported != "_"
        && (slice.len() >= 2 || alias.is_some())
    {
        out.insert(exported.to_owned(), layer.clone());
    }

    cursor
}

/// Extracts every crate-root identifier referenced via `crate::<Ident>` or `use crate::{...}`.
fn extract_crate_root_refs(tokens: &[Token]) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    let mut index = 0;

    while index < tokens.len() {
        if matches!(
            tokens.get(index..index + 3),
            Some([
                Token::Ident(crate_kw),
                Token::Punct(':'),
                Token::Punct(':')
            ]) if crate_kw == "crate"
        ) {
            let cursor = index + 3;
            match tokens.get(cursor) {
                Some(Token::Punct('{')) => {
                    index = parse_use_group_idents(tokens, cursor + 1, None, &mut refs);
                    continue;
                }
                Some(Token::Ident(ident)) => {
                    refs.insert(ident.clone());
                    index = cursor + 1;
                    continue;
                }
                _ => {}
            }
        }
        index += 1;
    }

    refs
}

/// Extracts identifiers reached by crossing exactly `expected_depth` `super::` prefixes.
fn extract_super_prefix_refs(tokens: &[Token], expected_depth: usize) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    if expected_depth == 0 {
        return refs;
    }

    let mut index = 0;
    while index < tokens.len() {
        let preceded_by_colon = index > 0 && matches!(tokens[index - 1], Token::Punct(':'));
        if !preceded_by_colon
            && matches!(
                tokens.get(index..index + 3),
                Some([
                    Token::Ident(super_kw),
                    Token::Punct(':'),
                    Token::Punct(':')
                ]) if super_kw == "super"
            )
        {
            let mut count = 0usize;
            let mut cursor = index;
            while matches!(
                tokens.get(cursor..cursor + 3),
                Some([
                    Token::Ident(super_kw),
                    Token::Punct(':'),
                    Token::Punct(':')
                ]) if super_kw == "super"
            ) {
                count += 1;
                cursor += 3;
            }
            if count == expected_depth {
                match tokens.get(cursor) {
                    Some(Token::Punct('{')) => {
                        index = parse_use_group_idents(tokens, cursor + 1, None, &mut refs);
                        continue;
                    }
                    Some(Token::Ident(ident)) => {
                        refs.insert(ident.clone());
                        index = cursor + 1;
                        continue;
                    }
                    _ => {}
                }
            }
            index = cursor;
            continue;
        }
        index += 1;
    }

    refs
}

fn parse_use_group_idents(
    tokens: &[Token],
    mut cursor: usize,
    allowed: Option<&BTreeSet<String>>,
    refs: &mut BTreeSet<String>,
) -> usize {
    let mut depth = 1usize;
    let mut expect_segment_head = true;

    while cursor < tokens.len() && depth > 0 {
        match &tokens[cursor] {
            Token::Punct('{') => {
                depth += 1;
                expect_segment_head = false;
            }
            Token::Punct('}') => {
                depth -= 1;
            }
            Token::Punct(',') if depth == 1 => {
                expect_segment_head = true;
            }
            Token::Ident(name) if depth == 1 && expect_segment_head => {
                if allowed.is_none_or(|set| set.contains(name)) {
                    refs.insert(name.clone());
                }
                expect_segment_head = false;
            }
            _ => {}
        }
        cursor += 1;
    }

    cursor
}

fn find_upward_layer_violations(
    production_tokens: &[Token],
    owner_layer: &str,
    depth_from_crate: usize,
    layers: &[&str],
    reexports: &BTreeMap<String, String>,
) -> Vec<String> {
    let Some(owner_rank) = layer_rank(layers, owner_layer) else {
        return Vec::new();
    };

    let mut root_refs = extract_crate_root_refs(production_tokens);
    root_refs.extend(extract_super_prefix_refs(
        production_tokens,
        depth_from_crate,
    ));

    let mut violations = Vec::new();
    for ident in root_refs {
        if let Some(target_rank) = layer_rank(layers, &ident) {
            if target_rank > owner_rank {
                violations.push(format!(
                    "`{owner_layer}` (L{owner_rank}) references explicit `crate::{ident}` (`{ident}`, L{target_rank})"
                ));
            }
        } else if let Some(target_layer) = reexports.get(&ident)
            && let Some(target_rank) = layer_rank(layers, target_layer)
            && target_rank > owner_rank
        {
            violations.push(format!(
                "`{owner_layer}` (L{owner_rank}) references crate-root re-export `crate::{ident}` from `{target_layer}` (L{target_rank})"
            ));
        }
    }

    violations
}

fn find_horizontal_feature_violations(
    production_tokens: &[Token],
    owner_feature: &str,
    depth_in_features: usize,
    sibling_features: &BTreeSet<String>,
) -> Vec<String> {
    if owner_feature == "host" || !sibling_features.contains(owner_feature) {
        return Vec::new();
    }

    let refs = extract_layer_submodule_refs(
        production_tokens,
        "features",
        sibling_features,
        depth_in_features,
    );
    let mut violations = Vec::new();
    for target in refs {
        if target == owner_feature || target == "host" {
            continue;
        }
        violations.push(format!(
            "sibling feature `{owner_feature}` references sibling feature `{target}`"
        ));
    }

    violations
}

fn build_layer_submodule_dependency_graph(
    core_src: &Path,
    layer: &str,
    excluded_paths: &BTreeSet<PathBuf>,
) -> ModuleGraph {
    let layer_dir = core_src.join(layer);
    let modules = discover_top_level_modules(&layer_dir, excluded_paths);
    let mut edges: BTreeMap<String, BTreeSet<String>> = modules
        .iter()
        .map(|name| (name.clone(), BTreeSet::new()))
        .collect();
    let mut edge_files: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();

    let mut files = Vec::new();
    collect_rs_files(&layer_dir, &mut files);
    files.sort();

    for file in files {
        if is_test_only_file(&file, core_src, excluded_paths) {
            continue;
        }
        let Ok(rel) = file.strip_prefix(&layer_dir) else {
            continue;
        };
        let Some(owner) = top_level_owner(rel) else {
            continue;
        };
        if !modules.contains(&owner) {
            continue;
        }

        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", file.display()));
        let production_tokens = strip_cfg_test_tokens(&tokenize_code(&source));
        let depth_in_layer = module_depth_from_root(rel);
        let refs =
            extract_layer_submodule_refs(&production_tokens, layer, &modules, depth_in_layer);
        let rel_display = rel.display().to_string();

        for target in refs {
            if target == owner {
                continue;
            }
            edges
                .entry(owner.clone())
                .or_default()
                .insert(target.clone());
            edge_files
                .entry((owner.clone(), target))
                .or_default()
                .insert(rel_display.clone());
        }
    }

    ModuleGraph {
        modules,
        edges,
        edge_files,
    }
}

fn extract_layer_submodule_refs(
    tokens: &[Token],
    layer: &str,
    submodules: &BTreeSet<String>,
    depth_in_layer: usize,
) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    let mut index = 0;

    while index < tokens.len() {
        // Pattern 1: `crate::<layer>::<submod>` or `crate::<layer>::{<submod>, ...}`
        if matches!(
            tokens.get(index..index + 6),
            Some([
                Token::Ident(crate_kw),
                Token::Punct(':'),
                Token::Punct(':'),
                Token::Ident(layer_kw),
                Token::Punct(':'),
                Token::Punct(':')
            ]) if crate_kw == "crate" && layer_kw == layer
        ) {
            let cursor = index + 6;
            match tokens.get(cursor) {
                Some(Token::Punct('{')) => {
                    index = parse_use_group_idents(tokens, cursor + 1, Some(submodules), &mut refs);
                    continue;
                }
                Some(Token::Ident(submod)) => {
                    if submodules.contains(submod) {
                        refs.insert(submod.clone());
                    }
                    index = cursor + 1;
                    continue;
                }
                _ => {}
            }
        }

        // Pattern 2: `use crate::{ ..., <layer>::<submod>::..., <layer>::{<submod>, ...}, ... }`
        if matches!(
            tokens.get(index..index + 4),
            Some([
                Token::Ident(crate_kw),
                Token::Punct(':'),
                Token::Punct(':'),
                Token::Punct('{')
            ]) if crate_kw == "crate"
        ) {
            index =
                parse_crate_group_layer_submodules(tokens, index + 4, layer, submodules, &mut refs);
            continue;
        }

        index += 1;
    }

    for super_ref in extract_super_prefix_refs(tokens, depth_in_layer) {
        if submodules.contains(&super_ref) {
            refs.insert(super_ref);
        }
    }

    refs
}

fn parse_crate_group_layer_submodules(
    tokens: &[Token],
    mut cursor: usize,
    layer: &str,
    submodules: &BTreeSet<String>,
    refs: &mut BTreeSet<String>,
) -> usize {
    let mut depth = 1usize;
    let mut expect_segment_head = true;

    while cursor < tokens.len() && depth > 0 {
        match &tokens[cursor] {
            Token::Punct('{') => {
                depth += 1;
                expect_segment_head = false;
                cursor += 1;
            }
            Token::Punct('}') => {
                depth -= 1;
                cursor += 1;
            }
            Token::Punct(',') if depth == 1 => {
                expect_segment_head = true;
                cursor += 1;
            }
            Token::Ident(name) if depth == 1 && expect_segment_head => {
                expect_segment_head = false;
                if name == layer
                    && matches!(
                        tokens.get(cursor + 1..cursor + 3),
                        Some([Token::Punct(':'), Token::Punct(':')])
                    )
                {
                    match tokens.get(cursor + 3) {
                        Some(Token::Ident(submod)) => {
                            if submodules.contains(submod) {
                                refs.insert(submod.clone());
                            }
                            cursor += 4;
                            continue;
                        }
                        Some(Token::Punct('{')) => {
                            cursor =
                                parse_use_group_idents(tokens, cursor + 4, Some(submodules), refs);
                            continue;
                        }
                        _ => {}
                    }
                }
                cursor += 1;
            }
            _ => {
                cursor += 1;
            }
        }
    }

    cursor
}

#[test]
fn lib_reexport_parser_maps_grouped_symbols_and_module_aliases_to_layers() {
    let sample = r#"
        pub mod platform;
        pub mod mihomo;
        pub mod storage;
        pub mod runtime;
        pub mod workflows;

        pub use runtime::service::ipc as service;
        pub use workflows::diagnostics::ai_network;
        pub use crate::platform::AppDataPaths;
        pub use mihomo::{
            MihomoError,
            proxy::ProxyCatalog as Catalog,
            tun::{self, TunConfig},
        };
        pub use storage::{ProfileStore, YamlOverrideStore};
        pub use workflows::{BackupManager, ServiceManager};
        pub use workflows::HiddenTrait as _;
        pub use std::path::PathBuf;

        #[cfg(test)]
        pub use workflows::TestOnlySymbol;
    "#;

    let tokens = strip_cfg_test_tokens(&tokenize_code(sample));
    let map = parse_crate_root_reexports(&tokens, &ZENCLASH_CORE_LAYERS);

    assert_eq!(map.get("service").map(String::as_str), Some("runtime"));
    assert_eq!(map.get("ai_network").map(String::as_str), Some("workflows"));
    assert_eq!(
        map.get("AppDataPaths").map(String::as_str),
        Some("platform")
    );
    assert_eq!(map.get("MihomoError").map(String::as_str), Some("mihomo"));
    assert_eq!(map.get("Catalog").map(String::as_str), Some("mihomo"));
    assert_eq!(map.get("tun").map(String::as_str), Some("mihomo"));
    assert_eq!(map.get("TunConfig").map(String::as_str), Some("mihomo"));
    assert_eq!(map.get("ProfileStore").map(String::as_str), Some("storage"));
    assert_eq!(
        map.get("ServiceManager").map(String::as_str),
        Some("workflows")
    );
    assert!(!map.contains_key("_"));
    assert!(!map.contains_key("PathBuf"));
    assert!(!map.contains_key("TestOnlySymbol"));
}

#[test]
fn five_layer_dag_detector_catches_explicit_and_reexport_upward_refs_and_ignores_cfg_test() {
    let reexports: BTreeMap<String, String> = [
        ("AppDataPaths", "platform"),
        ("MihomoError", "mihomo"),
        ("ProfileStore", "storage"),
        ("CoreSession", "runtime"),
        ("service", "runtime"),
        ("ServiceManager", "workflows"),
        ("ai_network", "workflows"),
    ]
    .into_iter()
    .map(|(sym, layer)| (sym.to_owned(), layer.to_owned()))
    .collect();

    let clean_storage_source = r#"
        use crate::platform::paths::AppDataPaths;
        use crate::{MihomoError, ProfileStore};
        use crate::storage::overrides::YamlOverrideStore;

        fn ok() {
            let _ = crate::AppDataPaths::default();
        }

        #[cfg(test)]
        mod tests {
            use crate::workflows::backup::BackupManager;
            use crate::ServiceManager;
            use crate::service::Client;
        }
    "#;
    let clean_tokens = strip_cfg_test_tokens(&tokenize_code(clean_storage_source));
    assert!(
        find_upward_layer_violations(
            &clean_tokens,
            "storage",
            2,
            &ZENCLASH_CORE_LAYERS,
            &reexports,
        )
        .is_empty(),
        "downward, same-layer, and #[cfg(test)] references must be allowed"
    );

    let violating_storage_source = r#"
        use crate::workflows::backup::BackupManager;
        use crate::{MihomoError, ServiceManager, runtime::CoreSession};

        fn bad() {
            let _ = crate::service::Client;
            let _ = crate::ai_network::probe;
            let _ = super::super::CoreSession;
        }

        #[cfg(test)]
        fn ignored_in_tests() {
            use crate::workflows::telemetry::LogMonitor;
        }
    "#;
    let violating_tokens = strip_cfg_test_tokens(&tokenize_code(violating_storage_source));
    let violations = find_upward_layer_violations(
        &violating_tokens,
        "storage",
        2,
        &ZENCLASH_CORE_LAYERS,
        &reexports,
    );
    assert_eq!(violations.len(), 6, "violations: {violations:?}");
    assert!(violations.iter().any(|v| v.contains("crate::workflows")));
    assert!(violations.iter().any(|v| v.contains("crate::runtime")));
    assert!(
        violations
            .iter()
            .any(|v| v.contains("crate::ServiceManager"))
    );
    assert!(violations.iter().any(|v| v.contains("crate::service")));
    assert!(violations.iter().any(|v| v.contains("crate::ai_network")));
    assert!(violations.iter().any(|v| v.contains("crate::CoreSession")));
}

#[test]
fn layer_submodule_ref_extractor_handles_crate_and_super_paths() {
    let sample = r#"
        use crate::workflows::backup::BackupManager;
        use crate::workflows::{capture::CapturePlan, telemetry};
        use crate::{
            platform::CoreKind,
            workflows::{operations::ProxyOperations, service_manager},
        };
        use super::profile_apply::ProfileApplication;
        use super::super::storage::ProfileStore;
    "#;
    let submods: BTreeSet<String> = [
        "backup",
        "capture",
        "telemetry",
        "operations",
        "service_manager",
        "profile_apply",
        "storage",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    let refs = extract_layer_submodule_refs(&tokenize_code(sample), "workflows", &submods, 1);
    let expected: BTreeSet<String> = [
        "backup",
        "capture",
        "telemetry",
        "operations",
        "service_manager",
        "profile_apply",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(refs, expected);
}

#[test]
fn horizontal_feature_detector_catches_cross_feature_refs_and_ignores_host_and_cfg_test() {
    let sibling_features: BTreeSet<String> = [
        "capture", "home", "kernel", "profiles", "settings", "traffic",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    // Nested file `features/kernel/dns.rs` (depth_in_features = 2).
    let violating_nested = r#"
        use crate::features::host::RuntimePage;
        use crate::features::settings::PreferenceForm;
        use super::mihomo::MihomoState;
        use super::super::profiles::ProfileWorkflow;

        #[cfg(test)]
        mod tests {
            use crate::features::capture::TunView;
            use super::super::traffic::TrafficRange;
        }
    "#;
    let nested_tokens = strip_cfg_test_tokens(&tokenize_code(violating_nested));
    let nested_violations =
        find_horizontal_feature_violations(&nested_tokens, "kernel", 2, &sibling_features);
    assert_eq!(
        nested_violations.len(),
        2,
        "violations: {nested_violations:?}"
    );
    assert!(nested_violations.iter().any(|v| v.contains("`settings`")));
    assert!(nested_violations.iter().any(|v| v.contains("`profiles`")));

    // Top-level feature file `features/kernel.rs` (depth_in_features = 1).
    let violating_top = r#"
        use crate::features::host::RuntimePage;
        use super::settings::PreferenceForm;

        #[cfg(test)]
        fn helper() {
            let _ = super::home::HomeUiState;
        }
    "#;
    let top_tokens = strip_cfg_test_tokens(&tokenize_code(violating_top));
    let top_violations =
        find_horizontal_feature_violations(&top_tokens, "kernel", 1, &sibling_features);
    assert_eq!(top_violations.len(), 1, "violations: {top_violations:?}");
    assert!(top_violations[0].contains("`settings`"));

    // Page router `features/host/view.rs` is explicitly allowed to aggregate sibling features.
    let host_router = r#"
        use crate::features::settings::PreferenceForm;
        use super::super::kernel::dns;
    "#;
    let host_tokens = strip_cfg_test_tokens(&tokenize_code(host_router));
    assert!(
        find_horizontal_feature_violations(&host_tokens, "host", 2, &sibling_features).is_empty(),
        "`features/host/*` must be allowed to aggregate sibling features"
    );
}
