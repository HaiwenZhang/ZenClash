//! Application-owned assets layered on top of the gpui-component icon bundle.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use gpui_component_assets::Assets as ComponentAssets;

/// Asset path for the monochrome `ZenClash` brand mark.
pub const ZENCLASH_MARK_PATH: &str = "icons/zenclash-mark.svg";

/// Combined application and gpui-component asset source.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == ZENCLASH_MARK_PATH {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icons/zenclash-mark.svg"
            ))));
        }

        ComponentAssets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut assets = ComponentAssets.list(path)?;
        if ZENCLASH_MARK_PATH.starts_with(path) {
            assets.push(ZENCLASH_MARK_PATH.into());
        }
        Ok(assets)
    }
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;

    use super::{Assets, ZENCLASH_MARK_PATH};

    #[test]
    fn application_assets_include_the_brand_mark() {
        let mark = Assets
            .load(ZENCLASH_MARK_PATH)
            .expect("brand mark asset should load")
            .expect("brand mark asset should exist");

        assert!(mark.starts_with(b"<svg"));
    }

    #[test]
    fn component_icons_remain_available() {
        assert!(Assets
            .load("icons/globe.svg")
            .expect("component asset lookup should succeed")
            .is_some());
    }
}
