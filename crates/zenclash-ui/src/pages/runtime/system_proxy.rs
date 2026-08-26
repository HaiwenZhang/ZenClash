use gpui::Entity;
use gpui_component::input::InputState;

use super::SystemProxyMode;

mod actions;
mod view;

pub(super) struct SystemProxyEditorState {
    mode: SystemProxyMode,
    host: Entity<InputState>,
    bypass: Entity<InputState>,
    pac_script: Entity<InputState>,
}

#[derive(Clone, Debug)]
struct SystemProxyForm {
    mode: SystemProxyMode,
    host: String,
    bypass: Vec<String>,
    pac_script: String,
}
