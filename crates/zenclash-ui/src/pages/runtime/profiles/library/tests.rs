use super::*;
use crate::pages::{
    Page,
    runtime::test_support::{RuntimePageFixture, open_runtime_page_window},
};
use gpui_kit::{AppContext, TestAppContext};

#[gpui_kit::test]
fn equal_catalog_generations_cannot_adopt_another_librarys_read(cx: &mut TestAppContext) {
    let fixture = RuntimePageFixture::new();
    let runtime = fixture.services().runtime;
    let (first_window, first) = open_runtime_page_window(cx, &fixture, Page::Profiles);
    let (second_window, second) = open_runtime_page_window(cx, &fixture, Page::Profiles);
    fixture.wait_for_initial_reads(cx, &first);
    fixture.wait_for_initial_reads(cx, &second);
    let (first_read, second_read) = cx.update(|cx| {
        let first_read = first.update(cx, |page, _| page.profiles.start_read(&runtime).unwrap());
        let second_read = second.update(cx, |page, _| page.profiles.start_read(&runtime).unwrap());
        assert_eq!(
            first.read(cx).profiles.generation,
            second.read(cx).profiles.generation
        );
        (first_read, second_read)
    });
    let foreign = runtime.block_on(first_read.finish());
    let own = runtime.block_on(second_read.finish());
    cx.update(|cx| {
        second.update(cx, |page, _| {
            assert!(page.profiles.complete_read(foreign).is_none());
            assert!(page.profiles.complete_read(own).unwrap().is_ok());
            assert_eq!(page.profiles.catalog().profiles.len(), 1);
            let current = page.profiles.catalog().clone();
            page.profiles
                .adopt_bootstrap(None, ProfileCatalog::default());
            assert_eq!(
                page.profiles.catalog().profiles.len(),
                current.profiles.len()
            );
            assert!(page.profiles.store().is_none());
        });
    });
    cx.update_window(first_window, |_, window, _| window.remove_window())
        .unwrap();
    cx.update_window(second_window, |_, window, _| window.remove_window())
        .unwrap();
}
