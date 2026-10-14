use crate::pages::runtime::test_support::{
    RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::*;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;
use std::fs;

#[gpui_kit::test]
fn keyboard_focus_and_delete_follow_the_same_override_after_reordering(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let mut records = Vec::new();
    for name in ["first", "second", "third"] {
        let source = fixture.root.join(format!("{name}.yaml"));
        fs::write(&source, "allow-lan: true\n").unwrap();
        let record = fixture.overrides.import_paths([source]).unwrap().remove(0);
        fixture.overrides.set_enabled(&record.id, false).unwrap();
        records.push(record);
    }
    let (window, page) = open(cx, &fixture, Page::Override);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let target = format!("override-delete:{}", records[1].id);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..40 {
            if window.find(target.clone()).focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find(target.clone()).focused(), Some(true));
        fixture.overrides.move_to(&records[0].id, 2).unwrap();
        page.update(cx, |page, cx| {
            page.overrides
                .replace_fixture_catalog(fixture.overrides.load().unwrap());
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(window.find(target.clone()).focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| page.overrides.catalog().items.len() == 2);
    let catalog = fixture.overrides.load().unwrap();
    assert!(
        !catalog
            .items
            .iter()
            .any(|record| record.id == records[1].id)
    );
    assert!(
        catalog
            .items
            .iter()
            .any(|record| record.id == records[0].id)
    );
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
