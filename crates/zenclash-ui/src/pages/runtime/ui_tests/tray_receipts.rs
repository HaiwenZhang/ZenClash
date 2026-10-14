use crate::pages::runtime::test_support::{
    RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::*;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;

#[gpui_kit::test]
fn a_committed_tray_receipt_keeps_the_pending_service_warning(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let profile = fixture
        .profiles
        .load()
        .unwrap()
        .active_profile()
        .unwrap()
        .clone();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            let receipt = page
                .application_commands
                .publish_test_outcome(
                    zenclash_core::ProfileApplyOutcome::CommittedButRuntimeUnknown {
                        source_version: (&profile).into(),
                        profile,
                        path: fixture.profile.clone(),
                        cause: zenclash_core::ProfileApplicationError::Task(
                            "commit reply lost".into(),
                        ),
                        runtime_version: fixture.core.generation(),
                    },
                )
                .unwrap();
            let warning = receipt.warning().unwrap();
            page.profile_activated_from_tray(receipt, cx);
            assert_eq!(page.notice.as_deref(), Some(warning.as_str()));
            assert_eq!(
                page.profiles.pending_finalization(),
                Some(fixture.core.generation())
            );
            assert_eq!(page.profile_path.as_ref(), Some(&fixture.profile));
        });
        window.render_frame(cx);
        window.find("confirm-service-profile");
        window.remove_window();
    })
    .unwrap();
}
