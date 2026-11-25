use gpui_kit::Entity;
use gpui_kit::component::input::InputState;

use zenclash_core::SystemProxyMode;

use gpui_kit::component::input::TextareaState;

mod actions;
mod view;

pub(in crate::features) struct SystemProxyEditorState {
    mode: SystemProxyMode,
    host: Entity<InputState>,
    bypass: Entity<TextareaState>,
    pac_script: Entity<TextareaState>,
}

#[derive(Clone, Debug)]
struct SystemProxyForm {
    mode: SystemProxyMode,
    host: String,
    bypass: Vec<String>,
    pac_script: String,
}

#[cfg(test)]
mod ui_tests;
