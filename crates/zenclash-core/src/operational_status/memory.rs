use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

pub(super) fn resident_memory(pid: u32) -> Option<u64> {
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    system
        .process(pid)
        .map(|process| process.memory())
        .filter(|bytes| *bytes > 0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn current_process_has_resident_memory_and_an_invalid_pid_is_unknown() {
        assert!(super::resident_memory(std::process::id()).is_some_and(|bytes| bytes > 0));
        assert_eq!(super::resident_memory(u32::MAX), None);
    }
}
