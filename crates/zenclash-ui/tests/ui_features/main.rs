//! Tier 2 black-box GPUI headless feature integration tests (`required-features = ["test-support"]`).
//!
//! Exercises production `RuntimePage` and `ProxiesPage` views through public APIs
//! and `zenclash_ui::test_support` fixtures with `TestWindowExt`.

mod capture_and_kernel;
mod connections_and_rules;
mod home_and_proxies;
mod profiles_and_overrides;
mod settings_and_network;
