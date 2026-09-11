use std::{env, error::Error, fs, path::PathBuf};

mod build_version;

use build_version::mihomo_version;

const WINDOWS_ICON: &str = "../../platforms/windows/ZenClash.ico";

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed={WINDOWS_ICON}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_version.rs");
    println!("cargo:rerun-if-env-changed=ZENCLASH_VERSION");
    println!("cargo:rerun-if-env-changed=ZENCLASH_BUNDLED_MIHOMO_VERSION");
    let version = env::var("ZENCLASH_VERSION").unwrap_or(env::var("CARGO_PKG_VERSION")?);
    if version.trim().is_empty() || version.chars().any(char::is_control) {
        return Err("Invalid ZenClash build version".into());
    }
    println!("cargo:rustc-env=ZENCLASH_BUILD_VERSION={version}");
    let bundled = match env::var("ZENCLASH_BUNDLED_MIHOMO_VERSION") {
        Ok(output) => mihomo_version(&output).ok_or("Invalid bundled Mihomo version output")?,
        Err(env::VarError::NotPresent) => String::new(),
        Err(error) => return Err(error.into()),
    };
    println!("cargo:rustc-env=ZENCLASH_BUILD_MIHOMO_VERSION={bundled}");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let icon_path = manifest_dir.join(WINDOWS_ICON).canonicalize()?;
    let resource_path = PathBuf::from(env::var("OUT_DIR")?).join("zenclash.rc");
    let escaped_icon_path = icon_path.to_string_lossy().replace('\\', "\\\\");

    fs::write(&resource_path, format!("1 ICON \"{escaped_icon_path}\"\n"))?;
    embed_resource::compile_for(&resource_path, ["zenclash"], embed_resource::NONE)
        .manifest_required()?;

    Ok(())
}
