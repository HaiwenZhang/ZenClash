use objc2_foundation::{MainThreadMarker, NSSize};

pub(super) fn prepare_app_icon(tray: &tray_icon::TrayIcon) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(status_item) = tray.ns_status_item() else {
        return;
    };
    if let Some(button) = status_item.button(mtm)
        && let Some(image) = button.image()
    {
        // Keep the 36px raster at 18pt so Retina retains its extra detail.
        image.setSize(NSSize::new(18., 18.));
        button.setImage(Some(&image));
    }
    tray.set_title(Some(""));
}
