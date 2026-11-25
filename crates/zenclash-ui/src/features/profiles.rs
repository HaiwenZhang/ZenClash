mod actions;
mod state;
mod view;
pub(crate) mod workflow;
mod yaml_editor;

pub(in crate::features) use state::ProfileFormDismissal;
pub(crate) use state::ProfileFormState;

mod library;
pub(in crate::features) use library::ProfileLibrary;

#[cfg(test)]
mod ui_tests;
