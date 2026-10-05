// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
#![cfg(all(feature = "standalone", feature = "test"))]

use std::time::Duration;
use zenclash_service::acquire_service_owner;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let _owner_guard = match acquire_service_owner().await {
        Ok(Some(guard)) => guard,
        Ok(None) => std::process::exit(2),
        Err(error) => {
            eprintln!("failed to acquire owner lock: {error}");
            std::process::exit(1);
        }
    };

    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}
