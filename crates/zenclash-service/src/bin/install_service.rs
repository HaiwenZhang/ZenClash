// Modified for the ZenClash v2.0 redesign; GPL-3.0-only.
//! Transition compatibility shim forwarding to `zenclash_service::install::run_install_cli`.
//!
//! New callers should prefer `zenclash-service install [args...]`.

fn main() -> anyhow::Result<()> {
    zenclash_service::install::run_install_cli()
}
