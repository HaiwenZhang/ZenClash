use tray_icon::Icon;

pub(super) fn traffic_icon(upload: u64, download: u64) -> Result<Icon, String> {
    const WIDTH: u32 = 22;
    const HEIGHT: u32 = 18;
    let mut rgba = vec![0; (WIDTH * HEIGHT * 4) as usize];
    let mut pixel = |x: u32, y: u32, alpha: u8| {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let offset = ((y * WIDTH + x) * 4) as usize;
        rgba[offset + 3] = alpha;
    };

    for y in 4..15 {
        pixel(5, y, 255);
        pixel(6, y, 255);
    }
    for step in 0..4 {
        for x in (5 - step)..=(6 + step) {
            pixel(x, 4 + step, 255);
        }
    }

    for y in 3..14 {
        pixel(15, y, 255);
        pixel(16, y, 255);
    }
    for step in 0..4 {
        for x in (15 - step)..=(16 + step) {
            pixel(x, 14 - step, 255);
        }
    }

    let up_height = activity_height(upload);
    let down_height = activity_height(download);
    for y in 0..up_height {
        pixel(0, 16 - y, 170);
        pixel(1, 16 - y, 170);
    }
    for y in 0..down_height {
        pixel(20, 16 - y, 170);
        pixel(21, 16 - y, 170);
    }

    Icon::from_rgba(rgba, WIDTH, HEIGHT).map_err(|error| error.to_string())
}

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
    for pixel in rgba.chunks_exact_mut(4) {
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

fn activity_height(bytes_per_second: u64) -> u32 {
    if bytes_per_second == 0 {
        return 1;
    }
    let magnitude = u64::BITS - bytes_per_second.leading_zeros();
    (magnitude / 2).clamp(2, 14)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_indicator_keeps_an_idle_baseline() {
        assert_eq!(activity_height(0), 1);
    }

    #[test]
    fn activity_indicator_grows_logarithmically() {
        assert!(activity_height(1024) > activity_height(1));
    }

    #[test]
    fn activity_indicator_caps_extreme_throughput() {
        assert_eq!(activity_height(u64::MAX), 14);
    }

    #[test]
    fn creates_valid_rgba_tray_icons() {
        assert!(traffic_icon(1024, 2048).is_ok());
    }
}
