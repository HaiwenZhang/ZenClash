//! Keep retained Kit menus at the Home surface's rem scale.
use gpui_kit::component::{ActiveTheme, Theme, menu::PopupMenu};
use gpui_kit::{Context, Pixels, Window};

#[derive(Clone, Copy)]
pub(super) struct HomeMenuSize {
    min_width_rems: f32,
    max_width_rems: f32,
    max_height_rems: f32,
}

impl HomeMenuSize {
    pub(super) const PROFILE_PICKER: Self = Self {
        min_width_rems: 15.,
        max_width_rems: 26.25,
        max_height_rems: 22.5,
    };
    pub(super) const MORE_GROUPS: Self = Self {
        min_width_rems: 15.,
        max_width_rems: 26.,
        max_height_rems: 22.,
    };
    pub(super) const MORE_NODES: Self = Self {
        min_width_rems: 20.,
        max_width_rems: 32.,
        max_height_rems: 22.,
    };

    pub(super) fn apply_to(
        self,
        menu: PopupMenu,
        window: &Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let mut base_font = cx.theme().font_size;
        cx.observe_global_in::<Theme>(window, move |menu, window, cx| {
            let current_font = cx.theme().font_size;
            if current_font == base_font {
                return;
            }
            base_font = current_font;

            // Kit 0.7 only exposes consuming size builders. Move the retained value
            // through an unmounted scratch entity, then return it to its original
            // entity synchronously. Focus, selection, scrolling and listeners stay
            // on the original owner; the scratch is never rendered or focused.
            let scratch = PopupMenu::build(window, cx, |mut empty, _, _| {
                std::mem::swap(menu, &mut empty);
                self.resolve(empty, current_font)
            });
            scratch.update(cx, |resized, _| std::mem::swap(menu, resized));
            cx.notify();
        })
        .detach();
        self.resolve(menu, base_font)
    }

    fn resolve(self, menu: PopupMenu, base_font: Pixels) -> PopupMenu {
        menu.min_w(base_font * self.min_width_rems)
            .max_w(base_font * self.max_width_rems)
            .max_h(base_font * self.max_height_rems)
    }
}
