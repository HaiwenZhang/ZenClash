// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
#![cfg(all(feature = "standalone", feature = "client"))]

mod installer_arguments;

#[cfg(feature = "test")]
mod common;
#[cfg(feature = "test")]
mod owner_lifecycle;
#[cfg(feature = "test")]
mod reliability;
#[cfg(feature = "test")]
mod stage_runtime;
#[cfg(feature = "test")]
mod start_and_parse;
