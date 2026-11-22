// Modified for the ZenClash v2.0 redesign; GPL-3.0-only.
//! Unified `zenclash-service` privileged binary (`run` | `install` | `uninstall` | `--version`).

fn main() -> anyhow::Result<()> {
    zenclash_service::daemon::run_cli()
}
