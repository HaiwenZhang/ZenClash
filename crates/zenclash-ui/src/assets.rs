//! Application-owned assets layered on top of the gpui-component icon bundle.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use gpui_component::IconNamed;
use gpui_component_assets::Assets as ComponentAssets;

/// Asset path for the monochrome `ZenClash` brand mark.
pub const ZENCLASH_MARK_PATH: &str = "icons/zenclash-mark.svg";
/// Asset path for the group icon used by the proxies sidebar destination.
pub const GROUP_ICON_PATH: &str = "icons/group.svg";
/// Asset path for the radio icon used by the connections sidebar destination.
pub const RADIO_ICON_PATH: &str = "icons/radio.svg";
/// Asset path for the ruler icon used by the rules sidebar destination.
pub const RULER_ICON_PATH: &str = "icons/ruler.svg";
/// Asset path for the house icon used by the home destination.
pub const HOUSE_ICON_PATH: &str = "icons/house.svg";
/// Asset path for the clockwise refresh icon used by refresh commands.
pub const REFRESH_CW_ICON_PATH: &str = "icons/refresh-cw.svg";
/// Asset path for the gauge icon used by delay-test commands.
pub const GAUGE_ICON_PATH: &str = "icons/gauge.svg";
/// Asset path for the square pointer icon used by selection commands.
pub const SQUARE_MOUSE_POINTER_ICON_PATH: &str = "icons/square-mouse-pointer.svg";
/// Asset path for the square exit icon used by export commands.
pub const SQUARE_ARROW_RIGHT_EXIT_ICON_PATH: &str = "icons/square-arrow-right-exit.svg";

/// Application-owned icons that are not included in gpui-component's bundle.
#[derive(Clone, Copy)]
pub enum AppIcon {
    /// Home destination.
    House,
    /// Refresh the current data clockwise.
    RefreshCw,
    /// Measure proxy latency.
    Gauge,
    /// Select an item with the pointer.
    SquareMousePointer,
    /// Export data from the application.
    SquareArrowRightExit,
}

impl IconNamed for AppIcon {
    fn path(self) -> SharedString {
        match self {
            Self::House => HOUSE_ICON_PATH,
            Self::RefreshCw => REFRESH_CW_ICON_PATH,
            Self::Gauge => GAUGE_ICON_PATH,
            Self::SquareMousePointer => SQUARE_MOUSE_POINTER_ICON_PATH,
            Self::SquareArrowRightExit => SQUARE_ARROW_RIGHT_EXIT_ICON_PATH,
        }
        .into()
    }
}

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
        if path == HOUSE_ICON_PATH {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icons/house.svg"
            ))));
        }
        if path == REFRESH_CW_ICON_PATH {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icons/refresh-cw.svg"
            ))));
        }
        if path == GAUGE_ICON_PATH {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icons/gauge.svg"
            ))));
        }
        if path == SQUARE_MOUSE_POINTER_ICON_PATH {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icons/square-mouse-pointer.svg"
            ))));
        }
        if path == SQUARE_ARROW_RIGHT_EXIT_ICON_PATH {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icons/square-arrow-right-exit.svg"
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
            HOUSE_ICON_PATH,
            REFRESH_CW_ICON_PATH,
            GAUGE_ICON_PATH,
            SQUARE_MOUSE_POINTER_ICON_PATH,
            SQUARE_ARROW_RIGHT_EXIT_ICON_PATH,
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

    use super::{
        Assets, GAUGE_ICON_PATH, GROUP_ICON_PATH, HOUSE_ICON_PATH, RADIO_ICON_PATH,
        REFRESH_CW_ICON_PATH, RULER_ICON_PATH, SQUARE_ARROW_RIGHT_EXIT_ICON_PATH,
        SQUARE_MOUSE_POINTER_ICON_PATH, ZENCLASH_MARK_PATH,
    };

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
        for path in [
            GROUP_ICON_PATH,
            RADIO_ICON_PATH,
            RULER_ICON_PATH,
            HOUSE_ICON_PATH,
            REFRESH_CW_ICON_PATH,
            GAUGE_ICON_PATH,
            SQUARE_MOUSE_POINTER_ICON_PATH,
            SQUARE_ARROW_RIGHT_EXIT_ICON_PATH,
        ] {
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
