use std::{collections::VecDeque, process::Output};

pub(super) fn output(stdout: &[u8], success: bool) -> Output {
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(if success { 0 } else { 1 << 8 })
    };
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(if success { 0 } else { 1 })
    };
    Output {
        status,
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    }
}

pub(super) struct CommandFixture {
    replies: VecDeque<Result<Output, String>>,
    pub(super) calls: Vec<(String, Vec<String>)>,
}

impl CommandFixture {
    pub(super) fn new(replies: [Result<Output, String>; 2]) -> Self {
        Self {
            replies: replies.into(),
            calls: Vec::new(),
        }
    }

    pub(super) fn read(&mut self, command: &str, args: &[&str]) -> Result<Output, String> {
        self.calls.push((
            command.to_owned(),
            args.iter().map(|arg| (*arg).to_owned()).collect(),
        ));
        self.replies
            .pop_front()
            .expect("observer issued an unexpected command")
    }
}
