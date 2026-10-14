#[cfg(windows)]
mod native;
mod uplink;

#[cfg(windows)]
pub(super) use native::detect;
