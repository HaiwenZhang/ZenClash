use gpui_kit::component::input::InputState;
use gpui_kit::{AppContext, Context, Entity, Window};
use std::collections::HashMap;
use zenclash_core::{ProfileCatalog, RemoteProfileRoute};

/// Input and editor state owned by the profiles page.
pub(crate) struct ProfileFormState {
    pub(in crate::pages::runtime) search: Entity<InputState>,
    _search_subscription: gpui_kit::Subscription,
    pub(in crate::pages::runtime) details_open: bool,
    pub(in crate::pages::runtime) advanced_open: bool,
    pub(in crate::pages::runtime) catalog_view: ProfileCatalogView,
    pub(in crate::pages::runtime) adding_subscription: bool,
    pub(in crate::pages::runtime) subscription_error: Option<String>,
    pub(in crate::pages::runtime) subscription_name: Entity<InputState>,
    pub(in crate::pages::runtime) subscription_url: Entity<InputState>,
    pub(super) subscription_user_agent: Entity<InputState>,
    pub(super) subscription_authorization: Entity<InputState>,
    pub(in crate::pages::runtime) subscription_route: RemoteProfileRoute,
    pub(in crate::pages::runtime) request_name: Entity<InputState>,
    pub(super) request_url: Entity<InputState>,
    pub(super) request_user_agent: Entity<InputState>,
    pub(super) request_authorization: Entity<InputState>,
    pub(super) request_timeout_seconds: Entity<InputState>,
    pub(super) update_cron: Entity<InputState>,
    pub(in crate::pages::runtime) editing_profile_id: Option<String>,
    pub(super) editing_route: RemoteProfileRoute,
    pub(super) editing_fixed_update_interval: bool,
}

impl ProfileFormState {
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut Context<'_, super::super::RuntimePage>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(zenclash_i18n::text("unified.profiles.search"))
        });
        let search_subscription = cx.subscribe(
            &search,
            |this, _, event: &gpui_kit::component::input::InputEvent, cx| {
                if matches!(event, gpui_kit::component::input::InputEvent::Change) {
                    let query = this.profiles.forms.search.read(cx).value().to_lowercase();
                    this.profiles
                        .forms
                        .catalog_view
                        .set_query(query, &this.profiles.catalog);
                    cx.notify();
                }
            },
        );
        Self {
            search,
            _search_subscription: search_subscription,
            details_open: false,
            advanced_open: false,
            catalog_view: ProfileCatalogView::default(),
            adding_subscription: false,
            subscription_error: None,
            subscription_name: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(zenclash_i18n::text("profiles.form.placeholder_name"))
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

    pub(in crate::pages::runtime) fn refresh_localized_placeholders(
        &self,
        window: &mut Window,
        cx: &mut Context<'_, super::super::RuntimePage>,
    ) {
        for (input, key) in [
            (&self.subscription_name, "profiles.form.placeholder_name"),
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
pub(in crate::pages::runtime) struct ProfileCatalogView {
    query: String,
    pub(super) filter: ProfileFilter,
    pub(super) page: usize,
    selected: Option<String>,
    active: Option<String>,
    lookup: HashMap<String, usize>,
    indices: Vec<usize>,
    pub(super) remote_count: usize,
    catalog_address: usize,
    catalog_length: usize,
    identities: Vec<String>,
}

impl ProfileCatalogView {
    pub(in crate::pages::runtime) fn prepare(&mut self, catalog: &ProfileCatalog) {
        self.lookup = catalog
            .profiles
            .iter()
            .enumerate()
            .map(|(index, profile)| (profile.id.clone(), index))
            .collect();
        self.catalog_address = catalog.profiles.as_ptr() as usize;
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
        self.catalog_address == catalog.profiles.as_ptr() as usize
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
        if self.catalog_address != catalog.profiles.as_ptr() as usize
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
mod tests {
    use super::*;
    use zenclash_core::{ProfileRecord, ProfileSource};

    fn catalog() -> ProfileCatalog {
        ProfileCatalog {
            active: Some("profile-0".into()),
            profiles: (0..26)
                .map(|index| ProfileRecord {
                    id: format!("profile-{index}"),
                    name: format!("Profile {index}"),
                    file_name: format!("profile-{index}.yaml"),
                    source: if index % 2 == 0 {
                        ProfileSource::Local {
                            original_path: format!("profile-{index}.yaml"),
                        }
                    } else {
                        ProfileSource::Remote {
                            url: "https://example.invalid/profile".into(),
                            user_agent: "clash.meta".into(),
                            options: Default::default(),
                        }
                    },
                    updated_at: 0,
                    size_bytes: 0,
                    auto_update: false,
                    update_interval_minutes: 60,
                    update_cron: None,
                    subscription: Default::default(),
                })
                .collect(),
        }
    }

    #[test]
    fn successful_activation_moves_inspector_but_refresh_preserves_manual_inspection() {
        let mut catalog = catalog();
        let mut view = ProfileCatalogView::default();
        view.prepare(&catalog);
        view.select("profile-1");
        view.prepare(&catalog);
        assert!(view.is_selected("profile-1"));

        catalog.active = Some("profile-2".into());
        view.prepare(&catalog);
        assert!(view.is_selected("profile-2"));
        assert_eq!(view.selected_index(), view.active_index());

        view.select("profile-3");
        view.prepare(&catalog);
        assert!(view.is_selected("profile-3"));
        assert_eq!(view.active_index(), Some(2));
    }

    #[test]
    fn inspecting_a_subscription_does_not_activate_it_and_filters_preserve_its_identity() {
        let catalog = catalog();
        let mut view = ProfileCatalogView::default();
        view.prepare(&catalog);
        view.select("profile-21");
        view.set_filter(ProfileFilter::Remote, &catalog);
        assert_eq!(view.selected_index(), Some(21));
        assert_eq!(catalog.active.as_deref(), Some("profile-0"));
        assert!(
            view.visible_indices()
                .iter()
                .all(|&index| catalog.profiles[index].is_remote())
        );
        view.set_page(1);
        assert_eq!(view.visible_indices(), &[25]);
        view.set_filter(ProfileFilter::Local, &catalog);
        assert_eq!(view.page, 0);
        assert!(
            view.visible_indices()
                .iter()
                .all(|&index| !catalog.profiles[index].is_remote())
        );
    }

    #[test]
    fn replaced_catalog_is_not_rendered_through_stale_indices_and_refresh_clamps_pages() {
        let catalog = catalog();
        let mut view = ProfileCatalogView::default();
        view.prepare(&catalog);
        view.set_page(2);
        assert!(view.is_current(&catalog));
        let mut replacement = catalog.clone();
        replacement.profiles.truncate(1);
        assert!(!view.is_current(&replacement));
        view.prepare(&replacement);
        assert!(view.is_current(&replacement));
        assert_eq!(view.page, 0);
        assert_eq!(view.visible_indices(), &[0]);
    }

    #[test]
    fn active_profile_lookup_rejects_replaced_reordered_and_changed_identities() {
        let mut catalog = catalog();
        let mut view = ProfileCatalogView::default();
        view.prepare(&catalog);
        assert_eq!(view.active_profile(&catalog).unwrap().id, "profile-0");
        assert!(view.active_profile(&catalog.clone()).is_none());
        catalog.profiles.swap(0, 1);
        assert!(view.active_profile(&catalog).is_none());
        view.prepare(&catalog);
        assert_eq!(view.active_profile(&catalog).unwrap().id, "profile-0");
        catalog.active = Some("profile-1".into());
        assert!(view.active_profile(&catalog).is_none());
        view.prepare(&catalog);
        assert_eq!(view.active_profile(&catalog).unwrap().id, "profile-1");
        catalog.active = Some("missing".into());
        view.prepare(&catalog);
        assert!(view.active_profile(&catalog).is_none());
        catalog.active = None;
        view.prepare(&catalog);
        assert!(view.active_profile(&catalog).is_none());
    }
}
