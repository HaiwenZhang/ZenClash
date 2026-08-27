//! Application-owned assets layered on top of the gpui-component icon bundle.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use gpui_component_assets::Assets as ComponentAssets;

/// Asset path for the monochrome `ZenClash` brand mark.
pub const ZENCLASH_MARK_PATH: &str = "icons/zenclash-mark.svg";
/// Asset path for the group icon used by the proxies sidebar destination.
pub const GROUP_ICON_PATH: &str = "icons/group.svg";
/// Asset path for the radio icon used by the connections sidebar destination.
pub const RADIO_ICON_PATH: &str = "icons/radio.svg";
/// Asset path for the ruler icon used by the rules sidebar destination.
pub const RULER_ICON_PATH: &str = "icons/ruler.svg";

/// Combined application and gpui-component asset source.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == ZENCLASH_MARK_PATH {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icons/zenclash-mark.svg"
            ))));
        }
        if path == GROUP_ICON_PATH {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icons/group.svg"
            ))));
        }
        if path == RADIO_ICON_PATH {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icons/radio.svg"
            ))));
        }
        if path == RULER_ICON_PATH {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icons/ruler.svg"
            ))));
        }

        ComponentAssets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut assets = ComponentAssets.list(path)?;
        for app_asset in [
            ZENCLASH_MARK_PATH,
            GROUP_ICON_PATH,
            RADIO_ICON_PATH,
            RULER_ICON_PATH,
        ] {
            if app_asset.starts_with(path) {
                assets.push(app_asset.into());
            }
        }
        Ok(assets)
    }
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;

    use super::{Assets, GROUP_ICON_PATH, RADIO_ICON_PATH, RULER_ICON_PATH, ZENCLASH_MARK_PATH};

    #[test]
    fn application_assets_include_the_brand_mark() {
        let mark = Assets
            .load(ZENCLASH_MARK_PATH)
            .expect("brand mark asset should load")
            .expect("brand mark asset should exist");

        assert!(mark.starts_with(b"<svg"));
    }

    #[test]
    fn application_assets_include_the_sidebar_icons() {
        for path in [GROUP_ICON_PATH, RADIO_ICON_PATH, RULER_ICON_PATH] {
            let icon = Assets
                .load(path)
                .expect("sidebar icon asset should load")
                .expect("sidebar icon asset should exist");

            assert!(icon.starts_with(b"<svg"));
        }
    }

    #[test]
    fn component_icons_remain_available() {
        assert!(
            Assets
                .load("icons/globe.svg")
                .expect("component asset lookup should succeed")
                .is_some()
        );
    }
}
