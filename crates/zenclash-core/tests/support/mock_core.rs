// ZenClash isolated IPC fixture, adapted 2026-10-05. GPL-3.0-only; see NOTICE.md.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    zenclash_core::service::test_core::run().await
}
