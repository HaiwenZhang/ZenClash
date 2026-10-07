//! Application-owned assets layered on top of the GPUI Kit icon bundle.

use std::borrow::Cow;

use gpui_kit::assets::Assets as ComponentAssets;
use gpui_kit::component::IconNamed;
use gpui_kit::{AssetSource, Result, SharedString};

/// Asset path for the monochrome `ZenClash` brand mark.
pub const ZENCLASH_MARK_PATH: &str = "icons/zenclash-mark.svg";
/// Shared full-color title-bar branding from the unified design source.
pub const ZENCLASH_LOGO_PATH: &str = "images/zenclash-logo.png";
/// Asset path for the layered group icon used by the proxies sidebar destination.
pub const GROUP_ICON_PATH: &str = "icons/group.svg";
/// Asset path for the linked chain icon used by the connections sidebar destination.
pub const RADIO_ICON_PATH: &str = "icons/radio.svg";
/// Asset path for the rule shield icon used by the rules sidebar destination.
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

/// Application-owned icons that are not included in GPUI Kit's bundle.
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

gpui_kit::assets::icon_assets!(
    LogActionAssets,
    [
        Clipboard,
        Upload,
        Trash,
        Laptop,
        Server,
        Globe,
        Pause,
        Play,
        Square,
        ArrowDownUp,
        FileText,
        NotebookTabs,
        Activity,
        Layers,
        List,
        ChartColumn
    ]
);

/// Combined application and GPUI Kit asset source.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == ZENCLASH_LOGO_PATH {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/zenclash-logo.png"
            ))));
        }
        let diagnostic_icon: Option<&'static [u8]> = match path {
            "icons/server.svg" => Some(include_bytes!("../assets/icons/server.svg")),
            "icons/clock.svg" => Some(include_bytes!("../assets/icons/clock.svg")),
            "icons/network.svg" => Some(include_bytes!("../assets/icons/network.svg")),
            _ => None,
        };
        if let Some(bytes) = diagnostic_icon {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
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

        if let Some(asset) = LogActionAssets.load(path)? {
            return Ok(Some(asset));
        }
        ComponentAssets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut assets = ComponentAssets.list(path)?;
        assets.extend(LogActionAssets.list(path)?);
        for app_asset in [
            "icons/server.svg",
            "icons/clock.svg",
            "icons/network.svg",
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
    use gpui_kit::AssetSource as _;

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
