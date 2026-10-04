fn main() -> std::process::ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    #[cfg(target_os = "macos")]
    if arguments
        .iter()
        .map(|argument| argument.as_os_str())
        .eq([std::ffi::OsStr::new("--query-host-pid")])
    {
        return match zenclash_service::query_service_host_pid() {
            Ok(pid) => {
                println!("{pid}");
                std::process::ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("service host query failed: {error}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    if arguments
        .iter()
        .map(|argument| argument.as_os_str())
        .eq([std::ffi::OsStr::new("--version")])
    {
        println!("zenclash-service {}", env!("CARGO_PKG_VERSION"));
        std::process::ExitCode::SUCCESS
    } else if arguments
        .iter()
        .map(|argument| argument.as_os_str())
        .eq([std::ffi::OsStr::new("run")])
    {
        match zenclash_service::run_service() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("service failed: {error}");
                std::process::ExitCode::FAILURE
            }
        }
    } else if arguments
        .iter()
        .map(|argument| argument.as_os_str())
        .eq([std::ffi::OsStr::new("--package-uninstall")])
    {
        match zenclash_service::run_package_uninstall() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("package service removal failed: {error}");
                std::process::ExitCode::FAILURE
            }
        }
    } else {
        match zenclash_service::run_maintenance(&arguments) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("service maintenance failed: {error}");
                std::process::ExitCode::FAILURE
            }
        }
    }
}
