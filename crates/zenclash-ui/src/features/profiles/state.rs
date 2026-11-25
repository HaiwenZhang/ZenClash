use gpui_kit::component::input::InputState;
use gpui_kit::{AppContext, Context, Entity, Window};
use std::collections::HashMap;
use zenclash_core::{ProfileCatalog, RemoteProfileRoute};

use super::yaml_editor::ProfileYamlEditorState;

pub(in crate::features) enum ProfileFormDismissal {
    RemoteEditor,
    Subscription,
}

/// Input and editor state owned by the profiles page.
pub(crate) struct ProfileFormState {
    pub(in crate::features) yaml_editor: ProfileYamlEditorState,
    pub(in crate::features) search: Entity<InputState>,
    _search_subscription: gpui_kit::Subscription,
    pub(in crate::features) details_open: bool,
    pub(in crate::features) catalog_view: ProfileCatalogView,
    pub(in crate::features) adding_subscription: bool,
    pub(in crate::features) subscription_error: Option<String>,
    pub(in crate::features) subscription_name: Entity<InputState>,
    pub(in crate::features) subscription_url: Entity<InputState>,
    pub(super) subscription_user_agent: Entity<InputState>,
    pub(super) subscription_authorization: Entity<InputState>,
    pub(in crate::features) subscription_route: RemoteProfileRoute,
    pub(in crate::features) request_name: Entity<InputState>,
    pub(in crate::features) request_url: Entity<InputState>,
    pub(super) request_user_agent: Entity<InputState>,
    pub(super) request_authorization: Entity<InputState>,
    pub(super) request_timeout_seconds: Entity<InputState>,
    pub(super) update_cron: Entity<InputState>,
    pub(in crate::features) editing_profile_id: Option<String>,
    pub(super) editing_route: RemoteProfileRoute,
    pub(super) editing_fixed_update_interval: bool,
}

impl ProfileFormState {
    pub(in crate::features) fn dismissible_form(&self) -> Option<ProfileFormDismissal> {
        if self.editing_profile_id.is_some() {
            Some(ProfileFormDismissal::RemoteEditor)
        } else if self.adding_subscription {
            Some(ProfileFormDismissal::Subscription)
        } else {
            None
        }
    }

    pub(in crate::features) fn close_subscription(&mut self) -> bool {
        let was_open = self.adding_subscription;
        self.adding_subscription = false;
        was_open
    }

    pub(crate) fn new(
        window: &mut Window,
        cx: &mut Context<'_, crate::features::host::RuntimePage>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(zenclash_i18n::text("unified.profiles.search"))
        });
        let search_subscription = cx.subscribe(
            &search,
            |this, _, event: &gpui_kit::component::input::InputEvent, cx| {
                if matches!(event, gpui_kit::component::input::InputEvent::Change) {
                    let query = this.profiles.forms.search.read(cx).value().to_lowercase();
                    this.profiles.set_catalog_query(query);
                    cx.notify();
                }
            },
        );
        Self {
            yaml_editor: ProfileYamlEditorState::new(window, cx),
            search,
            _search_subscription: search_subscription,
            details_open: false,
            catalog_view: ProfileCatalogView::default(),
            adding_subscription: false,
            subscription_error: None,
            subscription_name: cx.new(|cx| {
                InputState::new(window, cx).placeholder(zenclash_i18n::text(
                    "settings_redesign.subscription_name_placeholder",
                ))
            }),
            subscription_url: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("https://example.com/api/v1/client/subscribe…")
            }),
            subscription_user_agent: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("clash.meta")
                    .placeholder("clash.meta")
            }),
            subscription_authorization: cx.new(|cx| {
                InputState::new(window, cx).placeholder(zenclash_i18n::text(
                    "profiles.form.placeholder_authorization",
                ))
            }),
            subscription_route: RemoteProfileRoute::DirectWithMihomoFallback,
            request_name: cx.new(|cx| {
                InputState::new(window, cx).placeholder(zenclash_i18n::text(
                    "profiles.form.placeholder_request_name",
                ))
            }),
            request_url: cx.new(|cx| {
                InputState::new(window, cx).placeholder("https://example.com/profile.yaml")
            }),
            request_user_agent: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("clash.meta")
                    .placeholder("clash.meta")
            }),
            request_authorization: cx.new(|cx| {
                InputState::new(window, cx).placeholder(zenclash_i18n::text(
                    "profiles.form.placeholder_request_authorization",
                ))
            }),
            request_timeout_seconds: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("30")
                    .placeholder("30")
            }),
            update_cron: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(zenclash_i18n::text("profiles.form.placeholder_cron"))
            }),
            editing_profile_id: None,
            editing_route: RemoteProfileRoute::DirectWithMihomoFallback,
            editing_fixed_update_interval: false,
        }
    }

    pub(in crate::features) fn refresh_localized_placeholders(
        &self,
        window: &mut Window,
        cx: &mut Context<'_, crate::features::host::RuntimePage>,
    ) {
        for (input, key) in [
            (
                &self.subscription_name,
                "settings_redesign.subscription_name_placeholder",
            ),
            (
                &self.subscription_authorization,
                "profiles.form.placeholder_authorization",
            ),
            (&self.request_name, "profiles.form.placeholder_request_name"),
            (
                &self.request_authorization,
                "profiles.form.placeholder_request_authorization",
            ),
            (&self.update_cron, "profiles.form.placeholder_cron"),
        ] {
            input.update(cx, |input, cx| {
                input.set_placeholder(zenclash_i18n::text(key), window, cx);
            });
        }
    }
}

const PROFILES_PER_PAGE: usize = 12;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) enum ProfileFilter {
    #[default]
    All,
    Remote,
    Local,
}

#[derive(Default)]
pub(in crate::features) struct ProfileCatalogView {
    query: String,
    pub(super) filter: ProfileFilter,
    pub(super) page: usize,
    selected: Option<String>,
    active: Option<String>,
    lookup: HashMap<String, usize>,
    indices: Vec<usize>,
    pub(super) remote_count: usize,
    catalog_fingerprint: u64,
    catalog_length: usize,
    identities: Vec<String>,
}

fn catalog_fingerprint(catalog: &ProfileCatalog) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    catalog.active.hash(&mut hasher);
    catalog.profiles.len().hash(&mut hasher);
    for profile in &catalog.profiles {
        profile.id.hash(&mut hasher);
        profile.name.hash(&mut hasher);
        profile.file_name.hash(&mut hasher);
        profile.is_remote().hash(&mut hasher);
        profile.updated_at.hash(&mut hasher);
        profile.size_bytes.hash(&mut hasher);
        profile.auto_update.hash(&mut hasher);
        profile.update_interval_minutes.hash(&mut hasher);
        profile.update_cron.hash(&mut hasher);
        if let Some(usage) = &profile.subscription.usage {
            true.hash(&mut hasher);
            usage.upload.hash(&mut hasher);
            usage.download.hash(&mut hasher);
            usage.total.hash(&mut hasher);
            usage.expire.hash(&mut hasher);
        } else {
            false.hash(&mut hasher);
        }
    }
    hasher.finish()
}

impl ProfileCatalogView {
    pub(in crate::features) fn prepare(&mut self, catalog: &ProfileCatalog) {
        self.lookup = catalog
            .profiles
            .iter()
            .enumerate()
            .map(|(index, profile)| (profile.id.clone(), index))
            .collect();
        self.catalog_fingerprint = catalog_fingerprint(catalog);
        self.catalog_length = catalog.profiles.len();
        self.identities = catalog
            .profiles
            .iter()
            .map(|profile| profile.id.clone())
            .collect();
        let active_changed = self.active != catalog.active;
        self.active = catalog.active.clone();
        self.remote_count = catalog
            .profiles
            .iter()
            .filter(|profile| profile.is_remote())
            .count();
        if active_changed
            || self
                .selected
                .as_ref()
                .is_none_or(|id| !self.lookup.contains_key(id))
        {
            self.selected = self
                .active
                .clone()
                .filter(|id| self.lookup.contains_key(id))
                .or_else(|| catalog.profiles.first().map(|profile| profile.id.clone()));
        }
        self.rebuild_indices(catalog);
    }

    fn rebuild_indices(&mut self, catalog: &ProfileCatalog) {
        self.indices = catalog
            .profiles
            .iter()
            .enumerate()
            .filter(|(_, profile)| match self.filter {
                ProfileFilter::All => true,
                ProfileFilter::Remote => profile.is_remote(),
                ProfileFilter::Local => !profile.is_remote(),
            })
            .filter(|(_, profile)| {
                self.query.is_empty() || profile.name.to_lowercase().contains(&self.query)
            })
            .map(|(index, _)| index)
            .collect();
        self.page = self.page.min(self.page_count().saturating_sub(1));
    }

    pub(super) fn set_query(&mut self, query: String, catalog: &ProfileCatalog) {
        self.query = query;
        self.page = 0;
        self.rebuild_indices(catalog);
    }

    pub(super) fn set_filter(&mut self, filter: ProfileFilter, catalog: &ProfileCatalog) {
        self.filter = filter;
        self.page = 0;
        self.prepare(catalog);
    }

    pub(super) fn is_current(&self, catalog: &ProfileCatalog) -> bool {
        self.catalog_fingerprint == catalog_fingerprint(catalog)
            && self.catalog_length == catalog.profiles.len()
            && self.active == catalog.active
            && self.visible_indices().iter().all(|&index| {
                catalog
                    .profiles
                    .get(index)
                    .zip(self.identities.get(index))
                    .is_some_and(|(profile, identity)| profile.id == *identity)
            })
            && self.selected_index().is_none_or(|index| {
                catalog
                    .profiles
                    .get(index)
                    .is_some_and(|profile| Some(profile.id.as_str()) == self.selected.as_deref())
            })
    }

    pub(super) fn select(&mut self, id: &str) {
        if self.lookup.contains_key(id) {
            self.selected = Some(id.to_owned());
        }
    }

    pub(super) fn is_selected(&self, id: &str) -> bool {
        self.selected.as_deref() == Some(id)
    }

    pub(super) fn selected_index(&self) -> Option<usize> {
        self.selected
            .as_ref()
            .and_then(|id| self.lookup.get(id))
            .copied()
    }

    pub(super) fn active_index(&self) -> Option<usize> {
        self.active
            .as_ref()
            .and_then(|id| self.lookup.get(id))
            .copied()
    }

    pub(super) fn active_profile<'a>(
        &self,
        catalog: &'a ProfileCatalog,
    ) -> Option<&'a zenclash_core::ProfileRecord> {
        if self.catalog_fingerprint != catalog_fingerprint(catalog)
            || self.catalog_length != catalog.profiles.len()
            || self.active != catalog.active
        {
            return None;
        }
        let identity = self.active.as_deref()?;
        let profile = catalog.profiles.get(self.active_index()?)?;
        (profile.id == identity).then_some(profile)
    }

    pub(super) fn visible_indices(&self) -> &[usize] {
        let start = (self.page * PROFILES_PER_PAGE).min(self.indices.len());
        &self.indices[start..(start + PROFILES_PER_PAGE).min(self.indices.len())]
    }

    pub(super) fn page_count(&self) -> usize {
        self.indices.len().div_ceil(PROFILES_PER_PAGE)
    }

    pub(super) fn set_page(&mut self, page: usize) {
        self.page = page.min(self.page_count().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests;
