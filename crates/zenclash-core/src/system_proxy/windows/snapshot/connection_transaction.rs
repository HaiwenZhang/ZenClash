//! Conditional default/LAN option writes and exact original raw representation replay.

use super::write_plan::{DefaultConnectionWriteStep, plan_change, plan_restore};
use super::*;
use crate::system_proxy::native::{
    NativeProxyChange, ownership_changed, unstable_observation, verification_failure,
};

pub(super) trait DefaultConnectionAccess {
    fn read_values(&self) -> MihomoResult<InternetSettingsRegistryValues>;
    fn query_connection(&self) -> MihomoResult<DefaultConnectionOptions>;
    fn set_option(&self, option: &DefaultConnectionOption) -> MihomoResult<()>;
    fn restore_registry_value(
        &self,
        name: &str,
        value: Option<&RawRegistryValue>,
    ) -> MihomoResult<()>;
    fn refresh(&self) -> MihomoResult<()>;
}

pub(super) fn capture(access: &impl DefaultConnectionAccess) -> MihomoResult<WinInetProxySnapshot> {
    let read =
        || WinInetProxySnapshot::from_native(access.read_values()?, access.query_connection()?);
    let before = read()?;
    let after = read()?;
    if !before.same_state(&after)? {
        return Err(unstable_observation());
    }
    Ok(after)
}

pub(super) fn apply(
    access: &impl DefaultConnectionAccess,
    before: &WinInetProxySnapshot,
    change: &NativeProxyChange<'_>,
    record: impl FnMut(WinInetProxySnapshot),
) -> MihomoResult<WinInetProxySnapshot> {
    let steps = plan_change(before, change)?;
    execute_write_steps(access, before, steps, record)
}

pub(super) fn restore(
    access: &impl DefaultConnectionAccess,
    before: &WinInetProxySnapshot,
    previous: &WinInetProxySnapshot,
    record: impl FnMut(WinInetProxySnapshot),
) -> MihomoResult<WinInetProxySnapshot> {
    let steps = plan_restore(before, previous)?;
    let restored = execute_write_steps(access, before, steps, record)?;
    if !restored.same_state(previous)? {
        return Err(verification_failure());
    }
    Ok(restored)
}

fn execute_write_steps(
    access: &impl DefaultConnectionAccess,
    before: &WinInetProxySnapshot,
    steps: Vec<DefaultConnectionWriteStep>,
    mut record: impl FnMut(WinInetProxySnapshot),
) -> MihomoResult<WinInetProxySnapshot> {
    let mut current = before.clone();
    for step in steps {
        if !capture(access)?.same_state(&current)? {
            return Err(ownership_changed());
        }
        let next = step.predicted_snapshot(&current)?;
        record(next.clone());
        step.write(access)?;
        if !capture(access)?.same_state(&next)? {
            return Err(verification_failure());
        }
        current = next;
    }
    access.refresh()?;
    let after = capture(access)?;
    if !after.same_state(&current)? {
        return Err(verification_failure());
    }
    Ok(after)
}

#[cfg(test)]
mod tests;
