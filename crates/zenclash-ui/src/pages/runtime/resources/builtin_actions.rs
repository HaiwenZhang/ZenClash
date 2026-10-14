//! Original-runtime reads and guarded publication after built-in resource updates.
#[cfg(test)]
use super::super::Page;
#[cfg(test)]
use super::super::page_observation::ObservedPage;
use super::super::{Context, RuntimePage};

#[derive(Clone, Copy)]
pub(super) enum BuiltinResource {
    GeoData,
    ExternalUi,
}

impl RuntimePage {
    pub(super) fn update_builtin_resource(
        &mut self,
        resource: BuiltinResource,
        cx: &mut Context<Self>,
    ) {
        let supported = match resource {
            BuiltinResource::GeoData => self.core_kind.capabilities().geodata_update,
            BuiltinResource::ExternalUi => self.core_kind.capabilities().external_ui_update,
        };
        if !supported {
            self.error = Some(zenclash_i18n::text_with(
                "resources.errors.unsupported",
                &[("core", self.core_kind.display_name().to_owned())],
            ));
            cx.notify();
            return;
        }
        let (action, notice) = match resource {
            BuiltinResource::GeoData => (
                zenclash_core::CoreResourceAction::GeoData,
                "resources.notices.geodata",
            ),
            BuiltinResource::ExternalUi => (
                zenclash_core::CoreResourceAction::ExternalUi,
                "resources.notices.external_ui",
            ),
        };
        self.start_resource_action(
            action,
            Some(zenclash_i18n::text(notice)),
            "resources.errors.builtin_task",
            cx,
        );
    }
}

#[cfg(test)]
mod tests;
