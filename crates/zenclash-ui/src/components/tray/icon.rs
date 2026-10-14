use tray_icon::Icon;

pub(super) fn app_icon() -> Result<Icon, String> {
    // Rasterize the existing brand asset at a native bitmap boundary; macOS
    // applies its own menu-bar tint to the resulting template image.
    let raster_size = if cfg!(target_os = "macos") { 36 } else { 18 };
    let renderer = gpui_kit::SvgRenderer::new(std::sync::Arc::new(crate::assets::Assets));
    let svg = include_str!("../../../assets/icons/zenclash-tray.svg");
    #[cfg(not(target_os = "macos"))]
    let svg = svg.replace("#000000", "#00BDA5");
    let parsed = renderer
        .parse_svg(svg.as_bytes())
        .map_err(|error| error.to_string())?;
    let image = renderer
        .render_parsed(
            &parsed,
            gpui_kit::SvgSize::ExactSize(gpui_kit::size(
                gpui_kit::DevicePixels(raster_size),
                gpui_kit::DevicePixels(raster_size),
            )),
        )
        .map_err(|error| error.to_string())?;
    let bytes = image
        .as_bytes(0)
        .ok_or_else(|| "application mark has no image frame".to_owned())?;
    let mut rgba = bytes.to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u32::from(*channel) * 255)
                .checked_div(alpha)
                .unwrap_or_default()
                .min(255) as u8;
        }
    }
    Icon::from_rgba(rgba, raster_size as u32, raster_size as u32).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn application_icon_rasterizes_the_existing_brand_asset() {
        assert!(super::app_icon().is_ok());
    }
}
