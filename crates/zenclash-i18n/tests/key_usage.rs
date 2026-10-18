//! Guards literal translation keys against typos and misplaced namespaces.
//! Resolves explicit imports syntactically, including aliases. Dynamic key values
//! and macro expansion require their own tests; this is not Rust name resolution.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use serde_yaml::Value;
use syn::{Expr, Lit, Stmt, UseTree};

#[test]
fn every_literal_translation_key_exists() {
    let catalog = load_catalog();
    let mut keys = BTreeSet::new();
    for file in rust_sources() {
        if file.file_name() == Some(OsStr::new("key_usage.rs")) {
            continue;
        }
        let source = fs::read_to_string(&file).expect("Rust source should be readable");
        for key in literal_keys(&source) {
            keys.insert((file.clone(), key));
        }
    }

    assert!(
        !keys.is_empty(),
        "the scanner found no literal translation keys"
    );
    let missing = keys
        .into_iter()
        .filter(|(_, key)| !catalog.contains(key))
        .map(|(file, key)| format!("{}: {key}", file.display()))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "literal translation keys missing from the catalog:\n{}",
        missing.join("\n")
    );
}

#[test]
fn update_and_autostart_error_details_resolve_in_both_languages() {
    let keys = load_catalog()
        .into_iter()
        .filter(|key| {
            key.starts_with("core_update.details.") || key.starts_with("autostart.details.")
        })
        .collect::<Vec<_>>();
    assert!(!keys.is_empty());
    for key in keys {
        let chinese = zenclash_i18n::text_for(zenclash_i18n::ZH_CN, &key);
        let english = zenclash_i18n::text_for(zenclash_i18n::EN, &key);
        assert!(
            chinese.chars().any(is_cjk),
            "Chinese error detail did not resolve: {key}"
        );
        assert!(
            !english.chars().any(is_cjk),
            "English error detail contains untranslated Chinese: {key}"
        );
        assert_ne!(english, key, "English error detail did not resolve");
        assert_eq!(
            placeholders(&chinese),
            placeholders(&english),
            "placeholder mismatch: {key}"
        );
    }
}

fn is_cjk(character: char) -> bool {
    matches!(character, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
}

fn placeholders(text: &str) -> BTreeSet<&str> {
    text.split("%{")
        .skip(1)
        .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
        .collect()
}

fn load_catalog() -> BTreeSet<String> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
    let mut keys = BTreeSet::new();
    let entries = fs::read_dir(&directory).expect("locale directory should be readable");
    for entry in entries {
        let path = entry.expect("locale entry should be readable").path();
        if path.extension() != Some(OsStr::new("yml")) {
            continue;
        }
        let text = fs::read_to_string(&path).expect("locale file should be readable");
        let document: Value = serde_yaml::from_str(&text).expect("locale YAML should parse");
        collect_keys(&document, &mut Vec::new(), &mut keys);
    }
    keys
}

fn collect_keys(value: &Value, prefix: &mut Vec<String>, keys: &mut BTreeSet<String>) {
    let Some(mapping) = value.as_mapping() else {
        return;
    };
    let mut has_zh = false;
    let mut has_en = false;
    for (key, value) in mapping {
        match key.as_str() {
            Some("zh-CN") if value.is_string() => has_zh = true,
            Some("en") if value.is_string() => has_en = true,
            _ => {}
        }
    }
    if has_zh && has_en {
        keys.insert(prefix.join("."));
        return;
    }
    for (key, child) in mapping {
        let Some(key) = key.as_str() else {
            continue;
        };
        prefix.push(key.to_owned());
        collect_keys(child, prefix, keys);
        prefix.pop();
    }
}

fn rust_sources() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("crates");
    let mut sources = Vec::new();
    collect_rust_sources(&root, &mut sources);
    sources.sort();
    sources
}

fn collect_rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory).expect("source directory should be readable");
    for entry in entries {
        let path = entry.expect("source entry should be readable").path();
        if path.is_dir() {
            if path.file_name() == Some(OsStr::new("target")) {
                continue;
            }
            collect_rust_sources(&path, sources);
        } else if path.extension() == Some(OsStr::new("rs")) {
            sources.push(path);
        }
    }
}

fn literal_keys(source: &str) -> Vec<String> {
    let tokens: TokenStream = source.parse().expect("Rust source should tokenize");
    let tokens = tokens.into_iter().collect::<Vec<_>>();
    let mut imports = BTreeMap::new();
    collect_imports(&tokens, &mut imports);
    let mut keys = Vec::new();
    collect_calls(&tokens, &imports, &mut keys);
    keys
}

fn collect_imports(tokens: &[TokenTree], imports: &mut BTreeMap<String, Vec<String>>) {
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token, TokenTree::Ident(name) if name == "use") {
            let end = tokens[index..]
                .iter()
                .position(|token| is_punct(token, ';'));
            if let Some(end) = end {
                let declaration: TokenStream =
                    tokens[index..=index + end].iter().cloned().collect();
                if let Ok(item) = syn::parse2::<syn::ItemUse>(declaration) {
                    collect_use_tree(&item.tree, &[], imports);
                }
            }
        }
        if let TokenTree::Group(group) = token {
            collect_imports(&group.stream().into_iter().collect::<Vec<_>>(), imports);
        }
    }
}

fn collect_use_tree(
    tree: &UseTree,
    prefix: &[String],
    imports: &mut BTreeMap<String, Vec<String>>,
) {
    match tree {
        UseTree::Path(path) => {
            let mut prefix = prefix.to_vec();
            prefix.push(path.ident.to_string());
            collect_use_tree(&path.tree, &prefix, imports);
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_use_tree(tree, prefix, imports);
            }
        }
        UseTree::Name(name) => {
            let mut path = prefix.to_vec();
            if name.ident != "self" {
                path.push(name.ident.to_string());
            }
            if let Some(name) = path.last() {
                imports.insert(name.clone(), path);
            }
        }
        UseTree::Rename(rename) => {
            let mut path = prefix.to_vec();
            if rename.ident != "self" {
                path.push(rename.ident.to_string());
            }
            imports.insert(rename.rename.to_string(), path);
        }
        UseTree::Glob(_) if prefix == ["zenclash_i18n"] => {
            for name in ["text", "text_with", "text_for"] {
                imports.insert(name.into(), vec!["zenclash_i18n".into(), name.into()]);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn collect_calls(
    tokens: &[TokenTree],
    imports: &BTreeMap<String, Vec<String>>,
    keys: &mut Vec<String>,
) {
    for (index, token) in tokens.iter().enumerate() {
        if let TokenTree::Group(group) = token {
            collect_calls(
                &group.stream().into_iter().collect::<Vec<_>>(),
                imports,
                keys,
            );
        }
        let TokenTree::Ident(name) = token else {
            continue;
        };
        if index > 0 && (is_punct(&tokens[index - 1], ':') || is_punct(&tokens[index - 1], '.')) {
            continue;
        }
        let mut path = vec![name.to_string()];
        let mut end = index + 1;
        while end + 2 < tokens.len()
            && is_punct(&tokens[end], ':')
            && is_punct(&tokens[end + 1], ':')
        {
            let TokenTree::Ident(name) = &tokens[end + 2] else {
                break;
            };
            path.push(name.to_string());
            end += 3;
        }
        if let Some(import) = imports.get(&path[0]) {
            path.splice(..1, import.iter().cloned());
        }
        if path.len() != 2 || path[0] != "zenclash_i18n" {
            continue;
        }
        let key_index = match path[1].as_str() {
            "text" | "text_with" => 0,
            "text_for" => 1,
            _ => continue,
        };
        let Some(TokenTree::Group(arguments)) = tokens.get(end) else {
            continue;
        };
        if arguments.delimiter() != Delimiter::Parenthesis {
            continue;
        }
        // Groups keep commas inside nested calls/matches separate from the call's commas.
        let argument_tokens = arguments.stream();
        let mut arguments = vec![TokenStream::new()];
        for token in argument_tokens {
            if is_punct(&token, ',') {
                arguments.push(TokenStream::new());
            } else {
                arguments.last_mut().unwrap().extend([token]);
            }
        }
        if let Some(argument) = arguments.get(key_index)
            && let Ok(expression) = syn::parse2::<Expr>(argument.clone())
        {
            collect_key_expression(&expression, keys);
        }
    }
}

fn is_punct(token: &TokenTree, expected: char) -> bool {
    matches!(token, TokenTree::Punct(punctuation) if punctuation.as_char() == expected)
}

fn collect_key_expression(expression: &Expr, keys: &mut Vec<String>) {
    match expression {
        Expr::Lit(literal) => {
            if let Lit::Str(key) = &literal.lit {
                keys.push(key.value());
            }
        }
        Expr::Match(expression) => {
            for arm in &expression.arms {
                collect_key_expression(&arm.body, keys);
            }
        }
        Expr::Paren(expression) => collect_key_expression(&expression.expr, keys),
        Expr::Group(expression) => collect_key_expression(&expression.expr, keys),
        Expr::Block(expression) => collect_block_key(&expression.block, keys),
        Expr::If(expression) => {
            collect_block_key(&expression.then_branch, keys);
            if let Some((_, expression)) = &expression.else_branch {
                collect_key_expression(expression, keys);
            }
        }
        _ => {}
    }
}

fn collect_block_key(block: &syn::Block, keys: &mut Vec<String>) {
    if let Some(Stmt::Expr(expression, None)) = block.stmts.last() {
        collect_key_expression(expression, keys);
    }
}

#[test]
fn scanner_collects_match_arm_keys_in_error_attributes() {
    let source = r#"#[error("{}", zenclash_i18n::text(match self {
        Self::First => "first.key",
        Self::Second => { "second.key" },
    }))] struct Error;"#;
    assert_eq!(literal_keys(source), ["first.key", "second.key"]);
}

#[test]
fn scanner_resolves_function_and_module_aliases_and_constant_locales() {
    let source = r#"
        use zenclash_i18n::{self as i18n, text as tr, text_for as localized};
        fn example() {
            tr("first.key");
            i18n::text_with("second.key", &[]);
            localized(EN, "third.key");
            zenclash_i18n::text_for(locale(), "fourth.key");
        }
    "#;
    assert_eq!(
        literal_keys(source),
        ["first.key", "second.key", "third.key", "fourth.key"]
    );
}

#[test]
fn scanner_handles_raw_strings_whitespace_and_ignores_examples_in_comments_and_strings() {
    let source = r##"
        // zenclash_i18n::text("not.a.key")
        const EXAMPLE: &str = "zenclash_i18n::text(\"also.not.a.key\")";
        fn example() {
            zenclash_i18n :: text
                (r#"real.key"#);
        }
    "##;
    assert_eq!(literal_keys(source), ["real.key"]);
}

#[test]
fn scanner_collects_conditional_keys_from_glob_imports() {
    let source = r#"
        use zenclash_i18n::*;
        fn example() {
            text(if enabled { "enabled.key" } else { "disabled.key" });
        }
    "#;
    assert_eq!(literal_keys(source), ["enabled.key", "disabled.key"]);
}
