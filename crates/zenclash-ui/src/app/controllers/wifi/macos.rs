//! Minimal CoreLocation/CoreWLAN bridge. No scanning, location collection or subprocesses.
use objc2::{
    AnyThread, DefinedClass, define_class, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject},
};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[link(name = "CoreWLAN", kind = "framework")]
unsafe extern "C" {}

#[link(name = "CoreLocation", kind = "framework")]
unsafe extern "C" {}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. Callbacks only touch an atomic counter.
    #[unsafe(super = NSObject)]
    #[name = "ZenClashWifiDelegate"]
    #[ivars = Arc<AtomicU64>]
    struct WifiDelegate;
    // SAFETY: NSObjectProtocol has no additional requirements.
    unsafe impl NSObjectProtocol for WifiDelegate {}
    impl WifiDelegate {
        #[unsafe(method(ssidDidChangeForWiFiInterfaceWithName:))]
        fn ssid_changed(&self, _: &NSString) { self.ivars().fetch_add(1, Ordering::Relaxed); }
        #[unsafe(method(linkDidChangeForWiFiInterfaceWithName:))]
        fn link_changed(&self, _: &NSString) { self.ivars().fetch_add(1, Ordering::Relaxed); }
        #[unsafe(method(clientConnectionInterrupted))]
        fn interrupted(&self) { self.ivars().fetch_add(1, Ordering::Relaxed); }
        #[unsafe(method(clientConnectionInvalidated))]
        fn invalidated(&self) { self.ivars().fetch_add(1, Ordering::Relaxed); }
        #[unsafe(method(locationManagerDidChangeAuthorization:))]
        fn authorization_changed(&self, _: &AnyObject) { self.ivars().fetch_add(1, Ordering::Relaxed); }
    }
);

struct NativeMonitor {
    client: Retained<AnyObject>,
    location: Retained<AnyObject>,
    _delegate: Retained<WifiDelegate>,
    revision: Arc<AtomicU64>,
    registered: bool,
    services_enabled: bool,
}

#[derive(Default)]
pub(in crate::app) struct WifiMonitor {
    native: Option<NativeMonitor>,
}

impl WifiMonitor {
    pub(in crate::app) fn start(&mut self, request: bool) {
        if self.native.is_none() && MainThreadMarker::new().is_some() {
            self.native = NativeMonitor::new();
        }
        if request {
            self.request_if_needed();
        }
    }

    pub(in crate::app) fn stop(&mut self) {
        self.native = None;
    }

    pub(in crate::app) fn update_services(&mut self, enabled: bool) {
        if let Some(native) = &mut self.native {
            native.services_enabled = enabled;
        }
    }

    pub(in crate::app) fn request_access(&mut self, cx: &mut gpui::App) {
        self.start(false);
        if matches!(self.status_key(), "ssid.denied" | "ssid.services_disabled") {
            cx.open_url(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_LocationServices",
            );
        } else {
            self.request_if_needed();
        }
    }

    fn request_if_needed(&self) {
        if self.status_key() != "ssid.permission_needed" {
            return;
        }
        if let Some(native) = &self.native {
            // SAFETY: The instance is a CLLocationManager, retained and called on its creation thread.
            unsafe {
                let _: () = msg_send![&native.location, requestWhenInUseAuthorization];
            }
        }
    }

    pub(in crate::app) fn status_key(&self) -> &'static str {
        let Some(native) = &self.native else {
            return "ssid.disabled";
        };
        if !has_usage_description() {
            return "ssid.bundle_required";
        }
        // SAFETY: Signatures come from CLLocationManager.h; no location updates are requested.
        unsafe {
            let status: i32 = msg_send![&native.location, authorizationStatus];
            match status {
                0 => "ssid.permission_needed",
                1 | 2 => "ssid.denied",
                3 | 4 if !native.services_enabled => "ssid.services_disabled",
                3 | 4 if native.registered => "ssid.ready",
                _ => "ssid.unavailable",
            }
        }
    }

    pub(in crate::app) fn observation(&self) -> Option<u64> {
        matches!(self.status_key(), "ssid.ready" | "ssid.services_disabled").then(|| {
            self.native
                .as_ref()
                .map_or(0, |native| native.revision.load(Ordering::Relaxed))
        })
    }
}

impl NativeMonitor {
    fn new() -> Option<Self> {
        let wifi_class = AnyClass::get(c"CWWiFiClient")?;
        let location_class = AnyClass::get(c"CLLocationManager")?;
        let revision = Arc::new(AtomicU64::new(0));
        // SAFETY: The named system classes are linked above. Init, delegate and event signatures
        // match CoreWLAN/CLLocationManager headers. Delegates are retained for the full registration.
        unsafe {
            let client: Retained<AnyObject> = msg_send![wifi_class, sharedWiFiClient];
            let location: Retained<AnyObject> = msg_send![location_class, new];
            let delegate = WifiDelegate::alloc().set_ivars(revision.clone());
            let delegate: Retained<WifiDelegate> = msg_send![super(delegate), init];
            let _: () = msg_send![&client, setDelegate: &*delegate];
            let _: () = msg_send![&location, setDelegate: &*delegate];
            let ssid: bool = msg_send![&client, startMonitoringEventWithType: 2isize, error: std::ptr::null_mut::<*mut AnyObject>()];
            let link: bool = msg_send![&client, startMonitoringEventWithType: 5isize, error: std::ptr::null_mut::<*mut AnyObject>()];
            Some(Self {
                client,
                location,
                _delegate: delegate,
                revision,
                registered: ssid && link,
                services_enabled: true,
            })
        }
    }
}

impl Drop for NativeMonitor {
    fn drop(&mut self) {
        // SAFETY: Teardown runs on the UI thread before releasing the retained delegate.
        unsafe {
            let _: bool = msg_send![&self.client, stopMonitoringAllEventsAndReturnError: std::ptr::null_mut::<*mut AnyObject>()];
            let _: () = msg_send![&self.client, setDelegate: std::ptr::null::<AnyObject>()];
            let _: () = msg_send![&self.location, setDelegate: std::ptr::null::<AnyObject>()];
        }
    }
}

fn has_usage_description() -> bool {
    let Some(class) = AnyClass::get(c"NSBundle") else {
        return false;
    };
    // SAFETY: NSBundle returns a process singleton and an optional plist value.
    unsafe {
        let bundle: Retained<AnyObject> = msg_send![class, mainBundle];
        let key = NSString::from_str("NSLocationWhenInUseUsageDescription");
        let description: Option<Retained<AnyObject>> =
            msg_send![&bundle, objectForInfoDictionaryKey: &*key];
        description.is_some()
    }
}

/// Reads only the associated SSID, on a background worker after authorization.
pub(in crate::app) fn read_ssid() -> (Option<String>, bool) {
    objc2::rc::autoreleasepool(|_| {
        let Some(location) = AnyClass::get(c"CLLocationManager") else {
            return (None, false);
        };
        // SAFETY: locationServicesEnabled can block, so it is only queried by this background worker.
        let enabled: bool = unsafe { msg_send![location, locationServicesEnabled] };
        if !enabled {
            return (None, false);
        }
        let Some(class) = AnyClass::get(c"CWWiFiClient") else {
            return (None, true);
        };
        // SAFETY: CoreWLAN's shared client and interface selectors return nullable retained
        // Objective-C objects. No network scan or configuration mutation is performed.
        unsafe {
            let client: Retained<AnyObject> = msg_send![class, sharedWiFiClient];
            let interface: Option<Retained<AnyObject>> = msg_send![&client, interface];
            let Some(interface) = interface else {
                return (None, true);
            };
            let ssid: Option<Retained<NSString>> = msg_send![&interface, ssid];
            (
                ssid.map(|ssid| ssid.to_string())
                    .filter(|ssid| !ssid.is_empty()),
                true,
            )
        }
    })
}
