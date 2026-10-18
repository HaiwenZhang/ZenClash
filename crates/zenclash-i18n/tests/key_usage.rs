//! Guards literal translation keys against typos and misplaced namespaces.

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use serde_yaml::Value;

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
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
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
    let mut keys = Vec::new();
    for marker in [
        "zenclash_i18n::text_with(",
        "zenclash_i18n::text_for(",
        "zenclash_i18n::text(",
    ] {
        let mut rest = source;
        while let Some(index) = rest.find(marker) {
            let call = &rest[index + marker.len()..];
            if marker.contains("text_for") {
                if let Some(key) = text_for_key(call) {
                    keys.push(key);
                }
            } else if let Some((key, _)) = take_string(call) {
                keys.push(key);
            } else {
                keys.extend(match_keys(call));
            }
            rest = call;
        }
    }
    keys
}

/// Collects string keys from `text(match self { ... => "key", ... })` mappings.
fn match_keys(call: &str) -> Vec<String> {
    let call = call.trim_start();
    if !call.starts_with("match") {
        return Vec::new();
    }
    let Some(match_index) = call.find("match") else {
        return Vec::new();
    };
    let rest = &call[match_index + "match".len()..];
    let Some(open_offset) = rest.find('{') else {
        return Vec::new();
    };
    let block = &rest[open_offset + 1..];
    let mut depth = 1_u32;
    let mut end = None;
    for (index, character) in block.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = end else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    let mut rest = &block[..end];
    while let Some(index) = rest.find("=>") {
        let arm = &rest[index + 2..];
        if let Some((key, after)) = take_string(arm) {
            keys.push(key);
            rest = after;
        } else {
            rest = arm;
        }
    }
    keys
}

fn text_for_key(call: &str) -> Option<String> {
    let (_, rest) = take_string(call)?;
    let rest = rest.trim_start().strip_prefix(',')?.trim_start();
    let (key, _) = take_string(rest)?;
    Some(key)
}

fn take_string(input: &str) -> Option<(String, &str)> {
    let rest = input.trim_start().strip_prefix('"')?;
    let mut value = String::new();
    let mut characters = rest.char_indices();
    while let Some((index, character)) = characters.next() {
        match character {
            '\\' => {
                let (_, escaped) = characters.next()?;
                match escaped {
                    'n' => value.push('\n'),
                    'r' => value.push('\r'),
                    't' => value.push('\t'),
                    '"' => value.push('"'),
                    '\\' => value.push('\\'),
                    other => value.push(other),
                }
            }
            '"' => return Some((value, &rest[index + 1..])),
            other => value.push(other),
        }
    }
    None
}

#[test]
fn scanner_collects_match_arm_keys() {
    let source = r#"zenclash_i18n::text(match self {
        Self::First => "first.key",
        Self::Second => "second.key",
    })"#;
    assert_eq!(literal_keys(source), ["first.key", "second.key"]);
}
