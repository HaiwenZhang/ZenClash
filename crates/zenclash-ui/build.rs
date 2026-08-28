use std::{env, error::Error, fs, path::PathBuf};

const WINDOWS_ICON: &str = "../../platforms/windows/ZenClash.ico";

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed={WINDOWS_ICON}");

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
