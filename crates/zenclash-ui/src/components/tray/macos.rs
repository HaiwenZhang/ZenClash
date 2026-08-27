use std::{cell::RefCell, sync::Arc};

use objc2::rc::Retained;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{NSEventTrackingRunLoopMode, NSStatusItem};
use objc2_foundation::{
    MainThreadMarker, NSDefaultRunLoopMode, NSObject, NSObjectProtocol, NSRunLoop, NSString,
    NSTimer,
};
use zenclash_core::TrafficMonitor;

use super::traffic_title;

struct TrafficTimerIvars {
    status_item: RefCell<Retained<NSStatusItem>>,
    monitor: Arc<TrafficMonitor>,
    last_title: RefCell<String>,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and this class is
    // confined to AppKit's main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "ZenClashTrafficTimerTarget"]
    #[ivars = TrafficTimerIvars]
    struct TrafficTimerTarget;

    // SAFETY: `NSObjectProtocol` has no additional safety requirements.
    unsafe impl NSObjectProtocol for TrafficTimerTarget {}

    impl TrafficTimerTarget {
        // SAFETY: The selector has the single `NSTimer` argument supplied by
        // `NSTimer` target-selector callbacks.
        #[unsafe(method(refreshTraffic:))]
        fn refresh_traffic(&self, _timer: &NSTimer) {
            let title = traffic_title(&self.ivars().monitor.snapshot());
            if title == *self.ivars().last_title.borrow() {
                return;
            }

            let tooltip = format!("ZenClash · {title}");
            let native_title = NSString::from_str(&title);
            let tooltip = NSString::from_str(&tooltip);
            let mtm = self.mtm();
            if let Some(button) = self.ivars().status_item.borrow().button(mtm) {
                button.setTitle(&native_title);
                button.setToolTip(Some(&tooltip));
            }
            *self.ivars().last_title.borrow_mut() = title;
        }
    }
);

impl TrafficTimerTarget {
    fn new(
        mtm: MainThreadMarker,
        status_item: Retained<NSStatusItem>,
        monitor: Arc<TrafficMonitor>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TrafficTimerIvars {
            status_item: RefCell::new(status_item),
            monitor,
            last_title: RefCell::new(String::new()),
        });
        // SAFETY: The signature of `NSObject`'s `init` method is correct.
        unsafe { msg_send![super(this), init] }
    }
}

/// Keeps the native traffic title moving while an `NSMenu` tracks input.
pub(super) struct NativeTrafficUpdater {
    timer: Retained<NSTimer>,
    _target: Retained<TrafficTimerTarget>,
}

impl NativeTrafficUpdater {
    pub(super) fn new(
        status_item: Retained<NSStatusItem>,
        monitor: Arc<TrafficMonitor>,
    ) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or_else(|| {
            "native traffic updater must be created on the main thread".to_owned()
        })?;
        let target = TrafficTimerTarget::new(mtm, status_item, monitor);
        // SAFETY: `refreshTraffic:` exists on `TrafficTimerTarget`, accepts the
        // timer argument, and both the target and timer are retained below.
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                0.5,
                &target,
                sel!(refreshTraffic:),
                None,
                true,
            )
        };
        let run_loop = NSRunLoop::mainRunLoop();
        // SAFETY: The timer is registered on the main run loop. The explicit
        // event-tracking mode is what keeps it firing while a menu is open.
        unsafe {
            run_loop.addTimer_forMode(&timer, NSDefaultRunLoopMode);
            run_loop.addTimer_forMode(&timer, NSEventTrackingRunLoopMode);
        }
        timer.fire();

        Ok(Self {
            timer,
            _target: target,
        })
    }

    pub(super) fn set_status_item(&self, status_item: Retained<NSStatusItem>) {
        *self._target.ivars().status_item.borrow_mut() = status_item;
        self._target.ivars().last_title.borrow_mut().clear();
        self.timer.fire();
    }
}

impl Drop for NativeTrafficUpdater {
    fn drop(&mut self) {
        self.timer.invalidate();
    }
}
