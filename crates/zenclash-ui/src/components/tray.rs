use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use zenclash_core::{format_speed, TrafficSnapshot};

/// Native status-bar indicator. The arrows are rendered as a macOS template
/// image and the live upload/download rates are shown beside it.
pub struct NetworkTrayIcon {
    icon: TrayIcon,
    last_title: String,
}

impl NetworkTrayIcon {
    pub fn new() -> Result<Self, String> {
        let icon = traffic_icon(0, 0)?;
        let tray = TrayIconBuilder::new()
            .with_tooltip("ZenClash · Mihomo 网络流量")
            .with_title("↑ 0 B/s  ↓ 0 B/s")
            .with_icon(icon)
            .with_icon_as_template(true)
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            icon: tray,
            last_title: String::new(),
        })
    }

    pub fn update(&mut self, traffic: &TrafficSnapshot) {
        let title = if traffic.connected {
            format!(
                "↑ {}  ↓ {}",
                format_speed(traffic.upload),
                format_speed(traffic.download)
            )
        } else {
            "Mihomo 离线".into()
        };
        if title == self.last_title {
            return;
        }
        self.icon.set_title(Some(&title));
        let _ = self.icon.set_tooltip(Some(format!("ZenClash · {title}")));
        if let Ok(icon) = traffic_icon(traffic.upload, traffic.download) {
            let _ = self.icon.set_icon_with_as_template(Some(icon), true);
        }
        self.last_title = title;
    }

    pub fn set_visible(&self, visible: bool) {
        let _ = self.icon.set_visible(visible);
    }
}

fn traffic_icon(upload: u64, download: u64) -> Result<Icon, String> {
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

fn activity_height(bytes_per_second: u64) -> u32 {
    if bytes_per_second == 0 {
        return 1;
    }
    ((bytes_per_second as f64 + 1.0).log2() / 2.0)
        .round()
        .clamp(2.0, 14.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_indicator_uses_logarithmic_height() {
        assert_eq!(activity_height(0), 1);
        assert!(activity_height(1024) > activity_height(1));
        assert_eq!(activity_height(u64::MAX), 14);
    }

    #[test]
    fn creates_valid_rgba_tray_icons() {
        assert!(traffic_icon(1024, 2048).is_ok());
    }
}
