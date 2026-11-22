//! Shared GPUI application actions and declarative page routing metadata.
//!
//! This module intentionally has zero dependency on `crate::app`,
//! `crate::components`, or `crate::pages` so navigation controls and feature
//! pages can dispatch semantic commands without creating module cycles.

// GPUI's macro offers no hook for documenting its generated marker structs, so
// the exception is confined to this definition-only module.
#[allow(
    clippy::derive_partial_eq_without_eq,
    missing_docs,
    reason = "the gpui actions macro controls generated marker derives and documentation"
)]
mod action_types {
    use gpui_kit::actions;

    actions!(
        zenclash,
        [
            Quit,
            NavigateHome,
            NavigateSystemProxy,
            NavigateTun,
            NavigateProfiles,
            NavigateProxies,
            NavigateMihomo,
            NavigateConnections,
            NavigateDns,
            NavigateSniffer,
            NavigateLogs,
            NavigateRules,
            NavigateResources,
            NavigateOverride,
            NavigateNetwork,
            NavigateTraffic,
            NavigateSettings,
            SetRuleMode,
            SetGlobalMode,
            SetDirectMode,
            SetSystemTheme,
            SetLightTheme,
            SetDarkTheme,
            ShowTrafficIcon,
            HideTrafficIcon,
            ShowStatusMenu,
            ToggleSidebar,
        ]
    );
}

pub use action_types::*;

/// Vertical scrolling ownership contract for a workspace destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageScrollPolicy {
    /// The page owns its own bounded viewport scroll container (for example,
    /// virtualized or paginated data tables such as Connections, Rules, and Logs).
    ViewportOwned,
    /// The runtime workspace shell wraps the page body in a single vertical scroll area.
    FormScroll,
}

/// Declarative chrome, title, and scroll-ownership metadata for one destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageDescriptor {
    /// Stable non-localized route slug used for `ElementId`s and action routing.
    pub route: &'static str,
    /// Vertical scrolling ownership policy for the page body.
    pub scroll_policy: PageScrollPolicy,
    /// Whether the destination lives inside the shared Settings workspace chrome.
    pub is_settings: bool,
    /// Whether the workspace header displays the compact navigation label
    /// instead of a separate long-form page title.
    pub use_label_as_title: bool,
    /// Whether the workspace header renders the one-line page subtitle.
    pub show_subtitle: bool,
}

impl PageDescriptor {
    /// Returns `true` when the page owns its own vertical viewport scroll container.
    #[must_use]
    pub const fn owns_viewport_scroll(self) -> bool {
        matches!(self.scroll_policy, PageScrollPolicy::ViewportOwned)
    }
}

#[cfg(test)]
mod tests {
    use super::{PageDescriptor, PageScrollPolicy};

    #[test]
    fn page_descriptor_reports_viewport_scroll_ownership() {
        let viewport = PageDescriptor {
            route: "connections",
            scroll_policy: PageScrollPolicy::ViewportOwned,
            is_settings: false,
            use_label_as_title: false,
            show_subtitle: false,
        };
        let form = PageDescriptor {
            route: "settings",
            scroll_policy: PageScrollPolicy::FormScroll,
            is_settings: true,
            use_label_as_title: false,
            show_subtitle: false,
        };

        assert!(viewport.owns_viewport_scroll());
        assert!(!form.owns_viewport_scroll());
    }
}
