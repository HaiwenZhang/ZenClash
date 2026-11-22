//! CI static architecture boundary guards (Section 6.1 of the v2.0 redesign).
//!
//! Pure-Rust static analysis using only `std` (zero external crates) to enforce:
//! 1. Zero direct bidirectional top-level module cycles in `zenclash-core` and `zenclash-ui`.
//! 2. Full transitive closure of `zenclash-ui`'s `components` and `actions` never reaches
//!    `pages` or `app`.
//! 3. Zero runtime `ProfileStore::discover` / `YamlOverrideStore::discover` calls outside
//!    `zenclash-app/src/bootstrap/`.
//! 4. Zero GUI dependencies (`gpui-kit`, `tray-icon`, `objc2`) in `zenclash-app`.
//! 5. Disjoint `HOT_UPDATE_CONFIG_KEYS` and `FULL_RELOAD_CONFIG_KEYS` in `zenclash-core`.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use zenclash_core::{FULL_RELOAD_CONFIG_KEYS, HOT_UPDATE_CONFIG_KEYS};

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

    for crate_name in ["zenclash-core", "zenclash-ui"] {
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
        graph.modules.contains("actions"),
        "expected top-level `actions` module under {}",
        ui_src.display()
    );

    let forbidden = ["pages", "app"];
    let mut violations = Vec::new();

    for entry in ["components", "actions"] {
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
