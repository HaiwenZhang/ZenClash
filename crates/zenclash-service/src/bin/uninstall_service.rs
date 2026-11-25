// Modified for the ZenClash v2.0 redesign; GPL-3.0-only.
//! Transition compatibility shim forwarding to `zenclash_service::install::run_uninstall_cli`.
//!
//! New callers should prefer `zenclash-service uninstall [args...]`.

fn main() -> anyhow::Result<()> {
    zenclash_service::install::run_uninstall_cli()
}
